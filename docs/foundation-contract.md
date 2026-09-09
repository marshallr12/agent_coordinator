# First implementation slice: integration contract

This file coordinates parallel implementation, not a change to product scope.
Root owns workspace manifests/core types, server coordination.rs, migration 0002,
mutation.rs, integration tests, and final integration. Authentication work owns
server lib.rs/main.rs/state.rs/error.rs/auth.rs, migration 0001, auth tests and
systemd/proxy examples. CLI work owns client/cli crates. UI owns web assets.

## Shared Rust interface

- `state::AppState`: Clone, public `pool: sqlx::SqlitePool`, `config: Config`,
  `clock: Arc<dyn Clock>`; `now() -> i64` milliseconds; `open(Config) ->
  anyhow::Result<Self>`. Clock: Send + Sync + `fn now_ms(&self)->i64`.
- Config fields: `database_path: PathBuf`, `listen: SocketAddr`,
  `public_origin: String`, `allow_insecure_loopback: bool`. Default creates only
  loopback bindings; development HTTP requires explicit opt-in.
- `auth::Auth`: axum FromRequestParts<AppState>, public `actor: Actor`;
  `verify(&self, &mut SqliteConnection, now: i64) -> Result<Actor, AppError>`
  revalidates credential/session revocation inside mutation transaction.
- Actor: Clone, public string `id,name,kind,role`, optional string
  `credential_id,session_id`. kind human/agent; role admin/operator/agent.
  Valid supplied agent session headers populate session_id only with correct
  proof and matching active issuing credential. Humans have browser session ID
  there. Agent without session can read but not own an attempt.
- `error::AppError`: `new(status: StatusCode, code: &str, message: &str)`,
  `bad_request(&str)`, `conflict(code:&str,message:&str)`, `forbidden(&str)`,
  `not_found()`, `auth_required()`, `with_details(Value)`;
  From<sqlx::Error>, serde_json::Error; IntoResponse JSON envelope. No secret Debug.
- `response(data: Value) -> axum::Json<Value>` produces
  `{data,request_id,server_time}`. UTC dates RFC3339. Root owns mutation.rs:
  `Mutation::begin(&AppState,&Auth,&HeaderMap,operation:&str,&impl Serialize)`;
  fields `tx: Transaction<'static,Sqlite>, actor:Actor, now:i64,
  replay:Option<Value>`; `finish(self,data:Value,project:Option<&str>,
  kind:&str,record_id:&str)->Result<Value,AppError>` returns data after commit.
  All writes use BEGIN IMMEDIATE, Auth::verify after lock, mandatory idempotency
  key, receipt and event in same tx. Replays still authenticate. Issued token
  receipt must be redacted; first response can attach secret after commit.
- lib router `pub fn router(state: AppState)->axum::Router` merges
  coordination::routes(), mounts public/assets/auth routes. Auth private
  extractors must run before decoding private bodies/project IDs; any global
  middleware needed for unknown private routes belongs in lib/auth.

## Database access foundation (migration 0001)

`principals(id TEXT PK,name TEXT UNIQUE,kind TEXT,role TEXT,password_hash TEXT
 NULL,disabled_at INTEGER NULL,created_at INTEGER)`;
`credentials(id TEXT PK,principal_id TEXT FK,token_hash TEXT UNIQUE,
 created_at INTEGER,revoked_at INTEGER NULL,expires_at INTEGER NULL)`;
`browser_sessions(id TEXT PK,principal_id TEXT FK,token_hash TEXT UNIQUE,
 expires_at INTEGER,revoked_at INTEGER NULL)`;
`agent_sessions(id TEXT PK,principal_id TEXT FK,credential_id TEXT FK,
 workstation_id TEXT,proof_hash TEXT,closed_at INTEGER NULL,created_at INTEGER,
 capabilities TEXT JSON,harness TEXT)`;
`events(seq INTEGER PK AUTOINCREMENT,project_id TEXT NULL,actor_id TEXT,
 kind TEXT,record_id TEXT,data_json TEXT,created_at INTEGER)`;
`mutation_receipts(principal_id TEXT,operation TEXT,key TEXT,fingerprint TEXT,
 result_json TEXT,created_at INTEGER,PRIMARY KEY(principal_id,operation,key))`.
Additional checks/indexes allowed. Secret hashing: SHA256 hex via shared
`auth::digest(&str)->String`; random token `auth::secret()->String`.
No agent API can mint full replacement tokens. Password hashing runs off async
threads. Embedded SQLx migrations include 0001 and root's later 0002.

## Routes and payloads (all success data is inside envelope.data)

Public GET /healthz, /api/v1/info, /api/v1/help/authentication.
GET /, /app.js, /style.css assets embedded by server.
POST /api/v1/auth/login `{username,password}` -> `{actor,csrf_token}` + HttpOnly
same-site cookie. GET /api/v1/me same response for browser restore. POST
/api/v1/auth/logout `{}`. Cookie writes need X-CSRF-Token and expected Origin.
Admin: GET /api/v1/admin/credentials -> `{items:[{id,name,revoked_at,...}]}`;
POST /api/v1/admin/agents `{name}` -> `{principal_id,credential_id,token}`;
POST /api/v1/admin/credentials/{id}/revoke `{}`.
Agent session: POST /api/v1/sessions `{session_id,workstation_id,harness,
capabilities:[]}` with X-Coordinator-Session-Proof (no session header yet);
GET /api/v1/sessions/{id}; POST /api/v1/sessions/{id}/close `{}`.
Existing session requests send X-Coordinator-Session and
X-Coordinator-Session-Proof in addition to Authorization Bearer token.

Root implements:
GET/POST /api/v1/projects (create `{name,repository_url,target_branch}`)
-> project `{id,name,repository_url,target_branch,policy_revision:1,
review_mode:"agent",lease_seconds:600,recovery_mode:"agent"}`;
GET/POST /api/v1/projects/{id}/tasks (create `{title,description,
acceptance_criteria:[string],kind:"code"|"general",priority:0..3,
depends_on:[task_id],planned:bool}`; default normal=2)
-> task `{id,project_id,title,description,acceptance_criteria,kind,priority,
lifecycle,revision,work_status,current_attempt_id,created_at}`;
GET /api/v1/projects/{id}/tasks/{task_id} -> task + attempts/checkpoints;
GET /api/v1/projects/{id}/orientation -> current project, instruction_version
"1",policy_revision,required_sections:["coordination-v1"], instructions text,
candidates,active_attempts, instructions_complete:true;
POST /api/v1/sessions/{id}/instruction-acknowledgments
`{project_id,policy_revision:1,instruction_version:"1",sections:["coordination-v1"]}`;
POST /api/v1/projects/{id}/claims `{task_id?,expected_task_revision?,mode:
"work"|"recovery",policy_revision:1,instruction_version:"1"}`; absent task_id
means next eligible; -> `{claim: {task,attempt,lease_remaining_ms},...}` or
`{claim:null,reasons:[...]}`. attempt includes id,task_id,owner_id,session_id,
generation,state,expires_at (RFC3339),last_heartbeat_at;
POST /api/v1/projects/{id}/attempts/{aid}/renew `{generation}`;
POST .../checkpoints `{generation,summary,current_action,next_step,blockers:[]}`;
POST .../release `{generation,summary,blocked:bool}`. Checkpoints/release don't
imply completion. Further workflow endpoints remain visibly unavailable in this
first slice; never provide an unrestricted status=done operation.
GET /api/v1/projects/{id}/events -> `{items:[...]}`.

Additional implemented routes:

- PATCH `/api/v1/projects/{id}/policy`: full replacement of editable policy
  fields with `expected_revision`, `review_mode`, `recovery_mode`, `lease_seconds`,
  `rules`, `agent_rule_editing`, and `automatic_integration`. Delegated agents
  cannot change the last two permission grants. The review/integration execution
  workflows remain unavailable in this foundation.
- PATCH `/api/v1/projects/{id}/tasks/{task_id}`: `expected_revision`, title,
  description, acceptance_criteria, priority, depends_on, and planned. Only an
  unowned open/planned task can be edited. There is no generic status edit.
- POST `.../tasks/{task_id}/unblock`: human-only `{expected_revision,reason}`.
- GET `.../attempts/{aid}`: attempt, task, current `authority_valid`, current
  `lease_remaining_ms`, and optional registered checkout metadata.
- POST `.../attempts/{aid}/checkout`: `{generation,workstation_id,identity,path,
  branch,base_revision,clean}`. Registers a client-reported separate clean checkout;
  the service does not execute Git or independently inspect the workstation.
- POST `.../attempts/{aid}/recovery-resolution`: `{generation,disposition:
  "resume"|"restart",summary,saved_work_checked:true,running_jobs_checked:true}`.
  Requires a recovery-mode attempt; only after this inspection may it resume
  normal work or release back to the queue. Incomplete inspections release blocked.

On a claim replay, `current_authority` reports whether the historical grant still
holds. The original claim is a receipt, not a fresh countdown. Renewal replay
returns current remaining time and requires a still-valid owned attempt. Checkpoint,
checkout, and recovery-resolution replays also require current ownership. Release
is the intentional exception: its historical receipt remains replayable after
release, because that operation ends ownership. Every replay reauthenticates.
All of these observations can become stale; ownership-dependent mutations still
enforce the current authority transactionally.

All lists `{items:[],next_cursor:null|opaque}`. Errors use stable error envelope.
The CLI must persist idempotency keys/requests before mutations, never retry an
uncertain mutation under a new key. New automatic claims are never connect's
side effect. Missing credentials can call public help. All routes not listed as
implemented are future milestones, not advertised working features.
