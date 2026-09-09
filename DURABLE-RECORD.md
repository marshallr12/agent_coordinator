# Durable engineering record

- **Claiming is a transaction.** A task listing reserves nothing. Sample server
  time and recheck authorization after acquiring the database writer lock; a
  request may have waited behind another writer until after its lease expired.
- **A receipt is historical evidence.** Replaying a renewal never resets its
  countdown. Claims report current authority separately; ownership-dependent
  checkpoint/checkout/recovery retries also revalidate the current attempt.
  Release retries can acknowledge the historical release because that operation
  deliberately ends ownership.
- **Session identity is distinct from a workstation token.** Separate harnesses
  need distinct persisted random proofs. Another harness using the same token
  still cannot write to an attempt it does not own.
- **The client participates in correctness.** Persist the exact mutation and key
  before sending, bind it to the credential/origin/session, lock across processes,
  and publish state durably. A malformed or missing success response is uncertainty,
  not permission to discard the original key.
- **Repository text is not a credential destination grant.** Bind environment
  tokens to an independently configured trusted origin, reject redirects, and
  refuse credential-bearing repository bindings. Otherwise changing a repository
  URL could redirect secrets to another service.
- **Operator views must use derived status.** A stored active attempt may be
  expired or revoked. Show recovery-required when authority is lost and do not
  label blocked/planned work available merely because it has no active owner.
- **State what is actually verified.** A Windows CI definition is not a passing
  Windows run. A deployment example is not a deployment. A checkout attestation is
  not remote filesystem inspection. A lease foundation is not reviewed integration.
- **A lease and a physical resource hold are different records.** Losing task
  authority or an observer never proves a producer stopped. Keep holds until
  terminal producer evidence or explicit operator termination/isolation evidence.
  Record reconciliation separately from the producer's reported outcome.
- **Launch identity must survive uncertainty.** Persist one producer identity and
  launch intent before spawning. Reconnect observes it; an unlocked guardian file
  or missing PID is not permission to start a replacement. Include boot/process
  creation identity so PID reuse cannot impersonate the original process.
- **Local observation must survive service failure.** Publish terminal results
  durably before uploading, preserve exact pending observation keys, and recover
  an interrupted journal write without dropping the result. Reading status must
  not wait for the lifetime lock held by a running guardian.
- **Historical registration is not launch permission.** Check current scoped
  launch authority and remaining lease time immediately before starting work.
  A successful replay cannot refresh an expired grant. Verify the actual checkout
  identity and clean source again before launch.
- **Log setup is part of launch correctness.** On Windows, an append-only handle
  cannot truncate a file. Initialize logs through a writable handle before launch
  intent, then append while draining. Persist pre-spawn failures as `not_started`
  with a safe local explanation so a detached guardian cannot fail silently.
- **Native identity paths and tool arguments have different requirements.** Keep
  canonical Windows paths for identity comparisons, but convert verbatim drive/UNC
  prefixes at the Git boundary. Git may reject the native extended path syntax.
