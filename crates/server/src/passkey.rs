//! Passkey (WebAuthn) registration and sign-in for human accounts.
//!
//! A signed-in person registers a passkey from My account: the service issues
//! a one-time challenge, the browser's authenticator signs it, and the service
//! stores only the credential id and public key. At the sign-in page the
//! browser asks the authenticator for any passkey it holds for this site
//! (discoverable credentials), and the service starts the same browser session
//! a password sign-in would. Only ES256 is accepted, no attestation is
//! requested, and user verification is required for both ceremonies.
use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, Method},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as B64};
use p256::ecdsa::{Signature, VerifyingKey, signature::Verifier};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{Row, SqliteConnection};

use crate::{
    auth::{self, Auth, issue_browser_session, require_origin, signed_in_response},
    error::AppError,
    mutation::Mutation,
    response,
    state::AppState,
};

/// How long a ceremony may take between its options and its result.
const CHALLENGE_TTL_MS: i64 = 5 * 60 * 1000;
/// Passkeys one account may hold.
const MAX_PASSKEYS: i64 = 10;
/// Unfinished sign-in ceremonies kept at once; the oldest are dropped first,
/// so anonymous requests cannot grow the table.
const MAX_SIGN_IN_CHALLENGES: i64 = 500;
const MAX_UNFINISHED_REGISTRATIONS: i64 = 20;

const FLAG_USER_PRESENT: u8 = 0x01;
const FLAG_USER_VERIFIED: u8 = 0x04;
const FLAG_ATTESTED: u8 = 0x40;
const COSE_ES256: i64 = -7;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/auth/passkeys",
            get(list_passkeys).post(register_passkey),
        )
        .route("/api/v1/auth/passkeys/options", post(registration_options))
        .route("/api/v1/auth/passkeys/{id}/delete", post(delete_passkey))
        .route("/api/v1/auth/passkey-login/options", post(sign_in_options))
        .route("/api/v1/auth/passkey-login", post(sign_in))
}

/// True for the two requests that start a session from a passkey. They carry
/// no credential: the signed challenge is the whole authority.
pub(crate) fn is_sign_in_request(method: &Method, path: &str) -> bool {
    *method == Method::POST
        && matches!(
            path,
            "/api/v1/auth/passkey-login/options" | "/api/v1/auth/passkey-login"
        )
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Empty {}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RegistrationInput {
    challenge_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    credential: RegistrationCredential,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RegistrationCredential {
    id: String,
    response: RegistrationResponse,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct RegistrationResponse {
    #[serde(rename = "clientDataJSON")]
    client_data_json: String,
    attestation_object: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SignInInput {
    challenge_id: String,
    credential: AssertionCredential,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AssertionCredential {
    id: String,
    response: AssertionResponse,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct AssertionResponse {
    #[serde(rename = "clientDataJSON")]
    client_data_json: String,
    authenticator_data: String,
    signature: String,
    #[serde(default)]
    user_handle: Option<String>,
}

fn invalid(message: &str) -> AppError {
    AppError::bad_request(message)
}
fn rejected() -> AppError {
    invalid("The passkey response was not accepted. Start again.")
}
fn decode(value: &str, limit: usize) -> Result<Vec<u8>, AppError> {
    if value.len() > limit * 2 {
        return Err(rejected());
    }
    B64.decode(value).map_err(|_| rejected())
}

/// The relying party id is the host of the configured public origin.
fn rp_id(state: &AppState) -> Result<String, AppError> {
    url::Url::parse(&state.config.public_origin)
        .ok()
        .and_then(|origin| origin.host_str().map(str::to_owned))
        .ok_or_else(AppError::internal)
}

fn random_challenge() -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("Operating system randomness unavailable");
    B64.encode(bytes)
}

async fn begin_unauthenticated(
    state: &AppState,
) -> Result<(sqlx::Transaction<'static, sqlx::Sqlite>, i64), AppError> {
    let mut tx = state.pool.begin_with("BEGIN IMMEDIATE").await?;
    let clock = state.sample_clock(&mut tx).await?;
    if clock.incident_detected {
        tx.commit().await?;
        return Err(crate::state::clock_reconciliation_error());
    }
    if clock.incident_active {
        return Err(crate::state::clock_reconciliation_error());
    }
    Ok((tx, clock.now))
}

/// Parses the client data every ceremony returns and checks it names this
/// ceremony, this challenge and this site.
fn check_client_data(
    state: &AppState,
    encoded: &str,
    kind: &str,
    challenge: &str,
) -> Result<Vec<u8>, AppError> {
    let raw = decode(encoded, 4096)?;
    let data: Value = serde_json::from_slice(&raw).map_err(|_| rejected())?;
    let matches = data["type"] == kind
        && data["challenge"] == challenge
        && data["origin"] == state.config.public_origin.as_str()
        && data["crossOrigin"] != json!(true);
    if !matches {
        return Err(rejected());
    }
    Ok(raw)
}

/// Splits authenticator data into its flags, counter and any trailing bytes
/// after checking the relying party hash and the required flags.
fn check_authenticator_data<'a>(
    state: &AppState,
    data: &'a [u8],
    attested: bool,
) -> Result<(u32, &'a [u8]), AppError> {
    if data.len() < 37 {
        return Err(rejected());
    }
    let flags = data[32];
    let required =
        FLAG_USER_PRESENT | FLAG_USER_VERIFIED | if attested { FLAG_ATTESTED } else { 0 };
    if data[..32] != Sha256::digest(rp_id(state)?.as_bytes())[..] || flags & required != required {
        return Err(rejected());
    }
    let counter = u32::from_be_bytes([data[33], data[34], data[35], data[36]]);
    Ok((counter, &data[37..]))
}

/// `POST /api/v1/auth/passkeys/options {}`: a challenge for registering a
/// passkey on the signed-in account.
async fn registration_options(
    State(state): State<AppState>,
    auth: Auth,
    Json(_): Json<Empty>,
) -> Result<Json<Value>, AppError> {
    auth.require_browser()?;
    let rp_id = rp_id(&state)?;
    let mut tx = state.pool.begin_with("BEGIN IMMEDIATE").await?;
    let clock = state.sample_clock(&mut tx).await?;
    if clock.incident_detected {
        tx.commit().await?;
        return Err(crate::state::clock_reconciliation_error());
    }
    if clock.incident_active {
        return Err(crate::state::clock_reconciliation_error());
    }
    let now = clock.now;
    let actor = auth.verify(&mut tx, now).await?;
    let existing =
        sqlx::query("SELECT credential_id FROM passkeys WHERE principal_id=? ORDER BY created_at")
            .bind(&actor.id)
            .fetch_all(&mut *tx)
            .await?;
    if existing.len() as i64 >= MAX_PASSKEYS {
        return Err(AppError::conflict(
            "passkey_limit",
            "This account already has the most passkeys it may hold. Remove one first.",
        ));
    }
    sqlx::query("DELETE FROM passkey_challenges WHERE expires_at<=?")
        .bind(now)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM passkey_challenges WHERE id IN (SELECT id FROM passkey_challenges WHERE principal_id=? AND purpose='register' ORDER BY created_at DESC LIMIT -1 OFFSET ?)")
        .bind(&actor.id)
        .bind(MAX_UNFINISHED_REGISTRATIONS - 1)
        .execute(&mut *tx)
        .await?;
    let id = uuid::Uuid::new_v4().to_string();
    let challenge = random_challenge();
    sqlx::query("INSERT INTO passkey_challenges(id,purpose,principal_id,challenge,created_at,expires_at) VALUES(?,'register',?,?,?,?)")
        .bind(&id)
        .bind(&actor.id)
        .bind(&challenge)
        .bind(now)
        .bind(now + CHALLENGE_TTL_MS)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    let excluded: Vec<Value> = existing
        .iter()
        .map(|row| json!({"type":"public-key","id":row.get::<String,_>("credential_id")}))
        .collect();
    Ok(response(json!({
        "challenge_id": id,
        "public_key": {
            "rp": {"id": rp_id, "name": "Agent Coordinator"},
            "user": {"id": B64.encode(actor.id.as_bytes()), "name": actor.name, "displayName": actor.name},
            "challenge": challenge,
            "pubKeyCredParams": [{"type":"public-key","alg":COSE_ES256}],
            "timeout": CHALLENGE_TTL_MS,
            "attestation": "none",
            "excludeCredentials": excluded,
            "authenticatorSelection": {"residentKey":"required","requireResidentKey":true,"userVerification":"required"},
        },
    })))
}

/// The credential id and ES256 key from attested credential data. Returns the
/// id and the key as an uncompressed SEC1 point.
fn attested_credential(data: &[u8]) -> Result<(Vec<u8>, Vec<u8>), AppError> {
    if data.len() < 18 {
        return Err(rejected());
    }
    let id_length = usize::from(u16::from_be_bytes([data[16], data[17]]));
    if id_length == 0 || id_length > 1023 || data.len() < 18 + id_length {
        return Err(rejected());
    }
    let id = data[18..18 + id_length].to_vec();
    let (key, _) = cbor::decode(&data[18 + id_length..]).map_err(|_| rejected())?;
    let field = |label: i64| key.get_int(label);
    let (Some(cbor::Item::Int(kty)), Some(cbor::Item::Int(alg)), Some(cbor::Item::Int(crv))) =
        (field(1), field(3), field(-1))
    else {
        return Err(rejected());
    };
    let (Some(cbor::Item::Bytes(x)), Some(cbor::Item::Bytes(y))) = (field(-2), field(-3)) else {
        return Err(rejected());
    };
    if *kty != 2 || *alg != COSE_ES256 || *crv != 1 || x.len() != 32 || y.len() != 32 {
        return Err(rejected());
    }
    let mut point = vec![4u8];
    point.extend_from_slice(x);
    point.extend_from_slice(y);
    // Rejects points that are not on the curve.
    VerifyingKey::from_sec1_bytes(&point).map_err(|_| rejected())?;
    Ok((id, point))
}

/// `POST /api/v1/auth/passkeys {challenge_id,name?,credential}`: verify the
/// authenticator's answer and keep the new passkey.
async fn register_passkey(
    State(state): State<AppState>,
    auth: Auth,
    headers: HeaderMap,
    Json(input): Json<RegistrationInput>,
) -> Result<Json<Value>, AppError> {
    auth.require_browser()?;
    if let Some(name) = &input.name {
        auth::validate_name(name)?;
    }
    let mut mutation = Mutation::begin(
        &state,
        &auth,
        &headers,
        "POST /api/v1/auth/passkeys",
        &input,
    )
    .await?;
    if let Some(replay) = &mutation.replay {
        return Ok(response(replay.clone()));
    }
    let challenge = take_challenge(
        &mut mutation.tx,
        &input.challenge_id,
        "register",
        mutation.now,
    )
    .await?
    .filter(|(principal, _)| principal.as_deref() == Some(mutation.actor.id.as_str()))
    .map(|(_, challenge)| challenge)
    .ok_or_else(|| invalid("The passkey request expired. Start again."))?;
    check_client_data(
        &state,
        &input.credential.response.client_data_json,
        "webauthn.create",
        &challenge,
    )?;
    let object = decode(&input.credential.response.attestation_object, 8192)?;
    let (object, _) = cbor::decode(&object).map_err(|_| rejected())?;
    let Some(cbor::Item::Bytes(authenticator_data)) = object.get_text("authData") else {
        return Err(rejected());
    };
    let (counter, attested) = check_authenticator_data(&state, authenticator_data, true)?;
    let (credential_id, public_key) = attested_credential(attested)?;
    if B64.encode(&credential_id) != input.credential.id {
        return Err(rejected());
    }
    let held: i64 = sqlx::query_scalar("SELECT count(*) FROM passkeys WHERE principal_id=?")
        .bind(&mutation.actor.id)
        .fetch_one(&mut *mutation.tx)
        .await?;
    if held >= MAX_PASSKEYS {
        return Err(AppError::conflict(
            "passkey_limit",
            "This account already has the most passkeys it may hold. Remove one first.",
        ));
    }
    let taken: i64 = sqlx::query_scalar("SELECT count(*) FROM passkeys WHERE credential_id=?")
        .bind(&input.credential.id)
        .fetch_one(&mut *mutation.tx)
        .await?;
    if taken != 0 {
        return Err(AppError::conflict(
            "passkey_exists",
            "That passkey is already registered.",
        ));
    }
    let id = uuid::Uuid::new_v4().to_string();
    let name = input
        .name
        .clone()
        .unwrap_or_else(|| format!("Passkey {}", held + 1));
    sqlx::query("INSERT INTO passkeys(id,principal_id,credential_id,public_key,sign_count,name,created_at) VALUES(?,?,?,?,?,?,?)")
        .bind(&id)
        .bind(&mutation.actor.id)
        .bind(&input.credential.id)
        .bind(B64.encode(&public_key))
        .bind(i64::from(counter))
        .bind(&name)
        .bind(mutation.now)
        .execute(&mut *mutation.tx)
        .await?;
    let created = auth::timestamp(mutation.now);
    Ok(response(
        mutation
            .finish(
                json!({"id":id,"name":name,"created_at":created}),
                None,
                "passkey_registered",
                &id,
            )
            .await?,
    ))
}

/// Removes and returns an unexpired challenge of the given purpose.
async fn take_challenge(
    connection: &mut SqliteConnection,
    id: &str,
    purpose: &str,
    now: i64,
) -> Result<Option<(Option<String>, String)>, AppError> {
    if !auth::valid_id(id) {
        return Ok(None);
    }
    let row = sqlx::query("DELETE FROM passkey_challenges WHERE id=? AND purpose=? RETURNING principal_id,challenge,expires_at")
        .bind(id)
        .bind(purpose)
        .fetch_optional(&mut *connection)
        .await?;
    Ok(row
        .filter(|row| row.get::<i64, _>("expires_at") > now)
        .map(|row| (row.get("principal_id"), row.get("challenge"))))
}

/// `POST /api/v1/auth/passkey-login/options {}`: a challenge for signing in
/// with whichever passkey the browser holds for this site.
async fn sign_in_options(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(_): Json<Empty>,
) -> Result<Json<Value>, AppError> {
    require_origin(&headers, &state)?;
    let rp_id = rp_id(&state)?;
    let (mut tx, now) = begin_unauthenticated(&state).await?;
    sqlx::query("DELETE FROM passkey_challenges WHERE expires_at<=?")
        .bind(now)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM passkey_challenges WHERE id IN (SELECT id FROM passkey_challenges WHERE purpose='sign_in' ORDER BY created_at DESC LIMIT -1 OFFSET ?)")
        .bind(MAX_SIGN_IN_CHALLENGES - 1)
        .execute(&mut *tx)
        .await?;
    let id = uuid::Uuid::new_v4().to_string();
    let challenge = random_challenge();
    sqlx::query("INSERT INTO passkey_challenges(id,purpose,principal_id,challenge,created_at,expires_at) VALUES(?,'sign_in',NULL,?,?,?)")
        .bind(&id)
        .bind(&challenge)
        .bind(now)
        .bind(now + CHALLENGE_TTL_MS)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(response(json!({
        "challenge_id": id,
        "public_key": {
            "challenge": challenge,
            "rpId": rp_id,
            "timeout": CHALLENGE_TTL_MS,
            "userVerification": "required",
            "allowCredentials": [],
        },
    })))
}

/// `POST /api/v1/auth/passkey-login {challenge_id,credential}`: verify the
/// signed challenge and start a browser session. Each challenge allows one
/// attempt, successful or not.
async fn sign_in(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<SignInInput>,
) -> Result<Response, AppError> {
    require_origin(&headers, &state)?;
    let (mut tx, now) = begin_unauthenticated(&state).await?;
    let challenge = take_challenge(&mut tx, &input.challenge_id, "sign_in", now).await?;
    let verified = match challenge {
        Some((_, challenge)) => verify_assertion(&state, &mut tx, &input, &challenge).await?,
        None => None,
    };
    let Some((passkey, counter, principal)) = verified else {
        tx.commit().await?;
        return Err(AppError::auth_required());
    };
    sqlx::query("UPDATE passkeys SET sign_count=?,last_used_at=? WHERE id=?")
        .bind(i64::from(counter))
        .bind(now)
        .bind(&passkey)
        .execute(&mut *tx)
        .await?;
    let (actor, token) = issue_browser_session(
        &mut tx,
        principal.get("id"),
        principal.get("name"),
        principal.get("role"),
        now,
        json!({"method":"passkey"}),
    )
    .await?;
    tx.commit().await?;
    Ok(signed_in_response(&state, actor, &token).into_response())
}

type Verified = (String, u32, sqlx::sqlite::SqliteRow);

/// Checks an assertion against the stored passkey and its enabled account.
/// `None` means the assertion is not acceptable.
async fn verify_assertion(
    state: &AppState,
    connection: &mut SqliteConnection,
    input: &SignInInput,
    challenge: &str,
) -> Result<Option<Verified>, AppError> {
    let response = &input.credential.response;
    let Ok(client_data) =
        check_client_data(state, &response.client_data_json, "webauthn.get", challenge)
    else {
        return Ok(None);
    };
    let (Ok(authenticator_data), Ok(signature)) = (
        decode(&response.authenticator_data, 1024),
        decode(&response.signature, 256),
    ) else {
        return Ok(None);
    };
    let Ok((counter, _)) = check_authenticator_data(state, &authenticator_data, false) else {
        return Ok(None);
    };
    let Some(row) = sqlx::query("SELECT k.id AS passkey_id,k.public_key,k.sign_count,p.id,p.name,p.role FROM passkeys k JOIN principals p ON p.id=k.principal_id WHERE k.credential_id=? AND p.kind='human' AND p.disabled_at IS NULL")
        .bind(&input.credential.id)
        .fetch_optional(&mut *connection)
        .await?
    else {
        return Ok(None);
    };
    // An authenticator that reports a counter must move it forward; one that
    // always reports zero (synced passkeys) is accepted.
    let stored = row.get::<i64, _>("sign_count");
    if (stored != 0 || counter != 0) && i64::from(counter) <= stored {
        return Ok(None);
    }
    if let Some(handle) = &response.user_handle
        && !handle.is_empty()
        && B64.decode(handle).ok().as_deref() != Some(row.get::<String, _>("id").as_bytes())
    {
        return Ok(None);
    }
    let key = B64
        .decode(row.get::<String, _>("public_key"))
        .ok()
        .and_then(|bytes| VerifyingKey::from_sec1_bytes(&bytes).ok());
    let (Some(key), Ok(signature)) = (key, Signature::from_der(&signature)) else {
        return Ok(None);
    };
    let mut signed = authenticator_data;
    signed.extend_from_slice(&Sha256::digest(&client_data));
    if key.verify(&signed, &signature).is_err() {
        return Ok(None);
    }
    Ok(Some((row.get("passkey_id"), counter, row)))
}

/// `GET /api/v1/auth/passkeys`: the signed-in account's passkeys.
async fn list_passkeys(State(state): State<AppState>, auth: Auth) -> Result<Json<Value>, AppError> {
    auth.require_browser()?;
    let rows = sqlx::query("SELECT id,name,created_at,last_used_at FROM passkeys WHERE principal_id=? ORDER BY created_at,id")
        .bind(&auth.actor.id)
        .fetch_all(&state.pool)
        .await?;
    let items: Vec<Value> = rows
        .iter()
        .map(|row| {
            json!({
                "id": row.get::<String,_>("id"),
                "name": row.get::<String,_>("name"),
                "created_at": auth::timestamp(row.get("created_at")),
                "last_used_at": row.get::<Option<i64>,_>("last_used_at").map(auth::timestamp),
            })
        })
        .collect();
    Ok(response(json!({"items":items})))
}

/// `POST /api/v1/auth/passkeys/{id}/delete {}`: forget one of the signed-in
/// account's passkeys.
async fn delete_passkey(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(input): Json<Empty>,
) -> Result<Json<Value>, AppError> {
    auth.require_browser()?;
    let mut mutation = Mutation::begin(
        &state,
        &auth,
        &headers,
        &format!("POST /api/v1/auth/passkeys/{id}/delete"),
        &input,
    )
    .await?;
    if let Some(replay) = &mutation.replay {
        return Ok(response(replay.clone()));
    }
    let removed = sqlx::query("DELETE FROM passkeys WHERE id=? AND principal_id=?")
        .bind(&id)
        .bind(&mutation.actor.id)
        .execute(&mut *mutation.tx)
        .await?
        .rows_affected();
    if removed == 0 {
        return Err(AppError::not_found());
    }
    Ok(response(
        mutation
            .finish(
                json!({"id":id,"deleted":true}),
                None,
                "passkey_deleted",
                &id,
            )
            .await?,
    ))
}

/// Just enough CBOR (RFC 8949) to read attestation objects and COSE keys:
/// definite lengths, integers, byte and text strings, arrays and maps.
mod cbor {
    const MAX_DEPTH: usize = 8;

    pub enum Item {
        Int(i64),
        Bytes(Vec<u8>),
        Text(String),
        Map(Vec<(Item, Item)>),
        /// Arrays and simple values: parsed to be skipped, never read.
        Other,
    }

    impl Item {
        pub fn get_text(&self, label: &str) -> Option<&Item> {
            let Item::Map(entries) = self else {
                return None;
            };
            entries
                .iter()
                .find(|(key, _)| matches!(key, Item::Text(text) if text == label))
                .map(|(_, value)| value)
        }
        pub fn get_int(&self, label: i64) -> Option<&Item> {
            let Item::Map(entries) = self else {
                return None;
            };
            entries
                .iter()
                .find(|(key, _)| matches!(key, Item::Int(number) if *number == label))
                .map(|(_, value)| value)
        }
    }

    /// Decodes the first item of `input` and returns it with the bytes used.
    pub fn decode(input: &[u8]) -> Result<(Item, usize), ()> {
        let mut at = 0;
        let item = item(input, &mut at, 0)?;
        Ok((item, at))
    }

    fn take<'a>(input: &'a [u8], at: &mut usize, count: usize) -> Result<&'a [u8], ()> {
        let end = at
            .checked_add(count)
            .filter(|end| *end <= input.len())
            .ok_or(())?;
        let slice = &input[*at..end];
        *at = end;
        Ok(slice)
    }

    fn item(input: &[u8], at: &mut usize, depth: usize) -> Result<Item, ()> {
        if depth > MAX_DEPTH {
            return Err(());
        }
        let head = take(input, at, 1)?[0];
        let (major, info) = (head >> 5, head & 0x1f);
        let argument = match info {
            0..=23 => u64::from(info),
            24 => u64::from(take(input, at, 1)?[0]),
            25 => u64::from(u16::from_be_bytes(
                take(input, at, 2)?.try_into().map_err(|_| ())?,
            )),
            26 => u64::from(u32::from_be_bytes(
                take(input, at, 4)?.try_into().map_err(|_| ())?,
            )),
            27 => u64::from_be_bytes(take(input, at, 8)?.try_into().map_err(|_| ())?),
            _ => return Err(()),
        };
        let length = || {
            usize::try_from(argument)
                .ok()
                .filter(|n| *n <= input.len())
                .ok_or(())
        };
        Ok(match major {
            0 => Item::Int(i64::try_from(argument).map_err(|_| ())?),
            1 => Item::Int(-1 - i64::try_from(argument).map_err(|_| ())?),
            2 => Item::Bytes(take(input, at, length()?)?.to_vec()),
            3 => {
                Item::Text(String::from_utf8(take(input, at, length()?)?.to_vec()).map_err(|_| ())?)
            }
            4 => {
                for _ in 0..length()? {
                    item(input, at, depth + 1)?;
                }
                Item::Other
            }
            5 => Item::Map(
                (0..length()?)
                    .map(|_| Ok((item(input, at, depth + 1)?, item(input, at, depth + 1)?)))
                    .collect::<Result<_, ()>>()?,
            ),
            7 if matches!(info, 20..=22) => Item::Other,
            _ => return Err(()),
        })
    }
}
