use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as B64};
use coordinator_server::{
    auth::init_admin,
    router,
    state::{AppState, Config},
};
use http_body_util::BodyExt;
use p256::ecdsa::{Signature, SigningKey, signature::Signer};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tower::ServiceExt;

const ORIGIN: &str = "https://coordinator.example.test";
const RP_ID: &str = "coordinator.example.test";
const PASSWORD: &str = "a lengthy test-only administrator password";

struct Fixture {
    state: AppState,
    _dir: tempfile::TempDir,
}
struct Reply {
    status: StatusCode,
    body: Value,
    cookie: Option<String>,
}
struct Browser {
    cookie: String,
    csrf: String,
}
impl Browser {
    fn headers<'a>(&'a self, key: &'a str) -> Vec<(&'a str, &'a str)> {
        vec![
            ("cookie", &self.cookie),
            ("origin", ORIGIN),
            ("x-csrf-token", &self.csrf),
            ("idempotency-key", key),
        ]
    }
}

/// A software authenticator: one ES256 key, a credential id and a counter.
struct Authenticator {
    key: SigningKey,
    credential_id: Vec<u8>,
    counter: u32,
}

fn cbor_head(major: u8, length: usize) -> Vec<u8> {
    match length {
        0..=23 => vec![major << 5 | length as u8],
        24..=255 => vec![major << 5 | 24, length as u8],
        _ => vec![major << 5 | 25, (length >> 8) as u8, length as u8],
    }
}
fn cbor_bytes(data: &[u8]) -> Vec<u8> {
    [cbor_head(2, data.len()), data.to_vec()].concat()
}
fn cbor_text(text: &str) -> Vec<u8> {
    [cbor_head(3, text.len()), text.as_bytes().to_vec()].concat()
}
fn rp_hash() -> Vec<u8> {
    Sha256::digest(RP_ID.as_bytes()).to_vec()
}
fn client_data(kind: &str, challenge: &str, origin: &str) -> Vec<u8> {
    json!({"type":kind,"challenge":challenge,"origin":origin})
        .to_string()
        .into_bytes()
}

impl Authenticator {
    fn new(seed: u8) -> Self {
        Self {
            key: SigningKey::from_slice(&[seed; 32]).unwrap(),
            credential_id: vec![seed; 16],
            counter: 0,
        }
    }
    fn id(&self) -> String {
        B64.encode(&self.credential_id)
    }
    fn cose_key(&self) -> Vec<u8> {
        let point = self.key.verifying_key().to_sec1_point(false);
        let mut key = vec![0xa5, 0x01, 0x02, 0x03, 0x26, 0x20, 0x01];
        key.extend([vec![0x21], cbor_bytes(point.x().unwrap())].concat());
        key.extend([vec![0x22], cbor_bytes(point.y().unwrap())].concat());
        key
    }
    fn create(&self, challenge: &str, flags: u8) -> Value {
        let mut data = rp_hash();
        data.push(flags);
        data.extend(self.counter.to_be_bytes());
        data.extend([0u8; 16]);
        data.extend((self.credential_id.len() as u16).to_be_bytes());
        data.extend(&self.credential_id);
        data.extend(self.cose_key());
        let mut object = vec![0xa3];
        object.extend(cbor_text("fmt"));
        object.extend(cbor_text("none"));
        object.extend(cbor_text("attStmt"));
        object.push(0xa0);
        object.extend(cbor_text("authData"));
        object.extend(cbor_bytes(&data));
        json!({"id":self.id(),"response":{
            "clientDataJSON":B64.encode(client_data("webauthn.create", challenge, ORIGIN)),
            "attestationObject":B64.encode(object),
        }})
    }
    fn assert(&mut self, challenge: &str, origin: &str, counter_step: u32) -> Value {
        self.counter += counter_step;
        let mut data = rp_hash();
        data.push(0x05);
        data.extend(self.counter.to_be_bytes());
        let client = client_data("webauthn.get", challenge, origin);
        let mut signed = data.clone();
        signed.extend(Sha256::digest(&client));
        let signature: Signature = self.key.sign(&signed);
        json!({"id":self.id(),"response":{
            "clientDataJSON":B64.encode(client),
            "authenticatorData":B64.encode(data),
            "signature":B64.encode(signature.to_der().as_bytes()),
            "userHandle":null,
        }})
    }
}

impl Fixture {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let state = AppState::open(Config {
            database_path: dir.path().join("passkey.sqlite3"),
            public_origin: ORIGIN.into(),
            ..Config::default()
        })
        .await
        .unwrap();
        init_admin(&state, "admin", PASSWORD.into()).await.unwrap();
        Self { state, _dir: dir }
    }
    async fn call(
        &self,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: Option<Value>,
    ) -> Reply {
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", "application/json");
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let body = body.map(|value| value.to_string()).unwrap_or_default();
        let result = router(self.state.clone())
            .oneshot(request.body(Body::from(body)).unwrap())
            .await
            .unwrap();
        let status = result.status();
        let cookie = result.headers().get("set-cookie").map(|value| {
            value
                .to_str()
                .unwrap()
                .split(';')
                .next()
                .unwrap()
                .to_owned()
        });
        let bytes = result.into_body().collect().await.unwrap().to_bytes();
        Reply {
            status,
            body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
            cookie,
        }
    }
    async fn login(&self) -> Browser {
        let result = self
            .call(
                "POST",
                "/api/v1/auth/login",
                &[("origin", ORIGIN)],
                Some(json!({"username":"admin","password":PASSWORD})),
            )
            .await;
        assert_eq!(result.status, StatusCode::OK);
        Browser {
            cookie: result.cookie.unwrap(),
            csrf: result.body["data"]["csrf_token"].as_str().unwrap().into(),
        }
    }
    async fn registration_challenge(&self, browser: &Browser) -> (String, String) {
        let options = self
            .call(
                "POST",
                "/api/v1/auth/passkeys/options",
                &browser.headers("unused"),
                Some(json!({})),
            )
            .await;
        assert_eq!(options.status, StatusCode::OK, "{:?}", options.body);
        let data = &options.body["data"];
        assert_eq!(data["public_key"]["rp"]["id"], RP_ID);
        assert_eq!(data["public_key"]["pubKeyCredParams"][0]["alg"], -7);
        (
            data["challenge_id"].as_str().unwrap().into(),
            data["public_key"]["challenge"].as_str().unwrap().into(),
        )
    }
    async fn register(&self, browser: &Browser, key: &str, authenticator: &Authenticator) -> Reply {
        let (challenge_id, challenge) = self.registration_challenge(browser).await;
        self.finish_registration(
            browser,
            key,
            &challenge_id,
            authenticator.create(&challenge, 0x45),
        )
        .await
    }
    async fn finish_registration(
        &self,
        browser: &Browser,
        key: &str,
        challenge_id: &str,
        credential: Value,
    ) -> Reply {
        self.call(
            "POST",
            "/api/v1/auth/passkeys",
            &browser.headers(key),
            Some(json!({"challenge_id":challenge_id,"name":"Test key","credential":credential})),
        )
        .await
    }
    async fn sign_in_challenge(&self) -> (String, String) {
        let options = self
            .call(
                "POST",
                "/api/v1/auth/passkey-login/options",
                &[("origin", ORIGIN)],
                Some(json!({})),
            )
            .await;
        assert_eq!(options.status, StatusCode::OK, "{:?}", options.body);
        let data = &options.body["data"];
        assert_eq!(data["public_key"]["rpId"], RP_ID);
        assert_eq!(data["public_key"]["allowCredentials"], json!([]));
        (
            data["challenge_id"].as_str().unwrap().into(),
            data["public_key"]["challenge"].as_str().unwrap().into(),
        )
    }
    async fn sign_in(&self, challenge_id: &str, credential: Value) -> Reply {
        self.call(
            "POST",
            "/api/v1/auth/passkey-login",
            &[("origin", ORIGIN)],
            Some(json!({"challenge_id":challenge_id,"credential":credential})),
        )
        .await
    }
}

#[tokio::test]
async fn registered_passkey_signs_in_once_per_challenge() {
    let fixture = Fixture::new().await;
    let mut authenticator = Authenticator::new(7);
    let browser = fixture.login().await;

    let created = fixture
        .register(&browser, "register-1", &authenticator)
        .await;
    assert_eq!(created.status, StatusCode::OK, "{:?}", created.body);
    let (challenge_id, challenge) = fixture.registration_challenge(&browser).await;
    let list = fixture
        .call("GET", "/api/v1/auth/passkeys", &browser.headers("x"), None)
        .await;
    assert_eq!(list.body["data"]["items"].as_array().unwrap().len(), 1);
    assert_eq!(list.body["data"]["items"][0]["name"], "Test key");
    // The same credential cannot be registered twice.
    let again = fixture
        .finish_registration(
            &browser,
            "register-2",
            &challenge_id,
            authenticator.create(&challenge, 0x45),
        )
        .await;
    assert_eq!(again.status, StatusCode::CONFLICT, "{:?}", again.body);

    let (challenge_id, challenge) = fixture.sign_in_challenge().await;
    let assertion = authenticator.assert(&challenge, ORIGIN, 1);
    let signed_in = fixture.sign_in(&challenge_id, assertion.clone()).await;
    assert_eq!(signed_in.status, StatusCode::OK, "{:?}", signed_in.body);
    assert_eq!(signed_in.body["data"]["actor"]["name"], "admin");
    let session = Browser {
        cookie: signed_in.cookie.unwrap(),
        csrf: signed_in.body["data"]["csrf_token"]
            .as_str()
            .unwrap()
            .into(),
    };
    let me = fixture
        .call("GET", "/api/v1/me", &[("cookie", &session.cookie)], None)
        .await;
    assert_eq!(me.status, StatusCode::OK);
    let list = fixture
        .call("GET", "/api/v1/auth/passkeys", &session.headers("x"), None)
        .await;
    assert!(list.body["data"]["items"][0]["last_used_at"].is_string());

    // The challenge is spent.
    let replay = fixture.sign_in(&challenge_id, assertion).await;
    assert_eq!(replay.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn forged_or_stale_assertions_are_refused() {
    let fixture = Fixture::new().await;
    let mut authenticator = Authenticator::new(9);
    let browser = fixture.login().await;
    let created = fixture
        .register(&browser, "register-1", &authenticator)
        .await;
    assert_eq!(created.status, StatusCode::OK, "{:?}", created.body);

    // Another site's origin.
    let (id, challenge) = fixture.sign_in_challenge().await;
    let wrong_origin = authenticator.assert(&challenge, "https://evil.example.test", 1);
    assert_eq!(
        fixture.sign_in(&id, wrong_origin).await.status,
        StatusCode::UNAUTHORIZED
    );

    // A challenge other than the issued one.
    let (id, _) = fixture.sign_in_challenge().await;
    let other = authenticator.assert("b3RoZXI", ORIGIN, 1);
    assert_eq!(
        fixture.sign_in(&id, other).await.status,
        StatusCode::UNAUTHORIZED
    );

    // A signature from a different key.
    let (id, challenge) = fixture.sign_in_challenge().await;
    let mut impostor = Authenticator::new(11);
    impostor.credential_id = authenticator.credential_id.clone();
    let forged = impostor.assert(&challenge, ORIGIN, 1);
    assert_eq!(
        fixture.sign_in(&id, forged).await.status,
        StatusCode::UNAUTHORIZED
    );

    // A counter that did not advance.
    let (id, challenge) = fixture.sign_in_challenge().await;
    let good = authenticator.assert(&challenge, ORIGIN, 1);
    assert_eq!(fixture.sign_in(&id, good).await.status, StatusCode::OK);
    let (id, challenge) = fixture.sign_in_challenge().await;
    let cloned = authenticator.assert(&challenge, ORIGIN, 0);
    assert_eq!(
        fixture.sign_in(&id, cloned).await.status,
        StatusCode::UNAUTHORIZED
    );

    // Unknown credential, unknown challenge.
    let (id, challenge) = fixture.sign_in_challenge().await;
    let mut stranger = Authenticator::new(13);
    let unknown = stranger.assert(&challenge, ORIGIN, 1);
    assert_eq!(
        fixture.sign_in(&id, unknown).await.status,
        StatusCode::UNAUTHORIZED
    );
    let unissued = authenticator.assert(&challenge, ORIGIN, 1);
    assert_eq!(
        fixture.sign_in("no-such-challenge", unissued).await.status,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn registration_requires_a_session_a_fresh_challenge_and_user_verification() {
    let fixture = Fixture::new().await;
    let authenticator = Authenticator::new(5);
    let anonymous = fixture
        .call(
            "POST",
            "/api/v1/auth/passkeys/options",
            &[("origin", ORIGIN)],
            Some(json!({})),
        )
        .await;
    assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);

    let browser = fixture.login().await;
    // User verification is required: presence plus attested data alone fails.
    let (id, challenge) = fixture.registration_challenge(&browser).await;
    let unverified = fixture
        .finish_registration(&browser, "k1", &id, authenticator.create(&challenge, 0x41))
        .await;
    assert_eq!(unverified.status, StatusCode::BAD_REQUEST);
    // The attempt did not use up the challenge, and a wrong one is refused.
    let wrong = fixture
        .finish_registration(&browser, "k2", &id, authenticator.create("d3Jvbmc", 0x45))
        .await;
    assert_eq!(wrong.status, StatusCode::BAD_REQUEST);
    let good = fixture
        .finish_registration(&browser, "k3", &id, authenticator.create(&challenge, 0x45))
        .await;
    assert_eq!(good.status, StatusCode::OK, "{:?}", good.body);
    // A spent challenge cannot be reused.
    let reused = fixture
        .finish_registration(&browser, "k4", &id, authenticator.create(&challenge, 0x45))
        .await;
    assert_eq!(reused.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn removed_passkey_no_longer_signs_in() {
    let fixture = Fixture::new().await;
    let mut authenticator = Authenticator::new(3);
    let browser = fixture.login().await;
    let created = fixture
        .register(&browser, "register-1", &authenticator)
        .await;
    let id = created.body["data"]["id"].as_str().unwrap().to_owned();
    let removed = fixture
        .call(
            "POST",
            &format!("/api/v1/auth/passkeys/{id}/delete"),
            &browser.headers("delete-1"),
            Some(json!({})),
        )
        .await;
    assert_eq!(removed.status, StatusCode::OK, "{:?}", removed.body);
    let (challenge_id, challenge) = fixture.sign_in_challenge().await;
    let assertion = authenticator.assert(&challenge, ORIGIN, 1);
    assert_eq!(
        fixture.sign_in(&challenge_id, assertion).await.status,
        StatusCode::UNAUTHORIZED
    );
}
