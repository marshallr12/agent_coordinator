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
