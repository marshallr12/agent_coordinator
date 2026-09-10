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
- **Publication intent is a boundary for retries.** Save intent before spawning
  Git, require fresh service authority and a conservative local deadline, and use
  an exact expected target for compare-and-swap. Once launch intent exists, retry
  observes; seeing an unchanged remote does not prove an old publisher stopped.
- **Checks verify an immutable source and definition.** Resolve registered producer
  receipts against the exact result commit/tree, check identity/version/environment,
  successful exit, and unchanged inputs. A generic successful job or reconciled
  unknown result cannot substitute for required evidence.
- **Recovery must preserve policy and target ownership together.** Releasing an old
  integration hold and creating its replacement in separate transactions permits
  competing integration or policy changes to strand an already published result.
  Transfer the hold atomically, retain historical publication evidence, and run
  fresh checks under the replacement activity.
- **Every claim path needs onboarding.** Review and integration claims must follow
  the same durable instruction acknowledgment flow as implementation claims. A
  reviewer may have connected without ever claiming an implementation task.

- **Build outputs belong to a source worktree.** Concurrent builds from different
  worktrees must use separate Cargo target directories. Shared compiled metadata
  can resolve a dependency against another worktree's version and produce
  misleading missing-type errors during integration.
- **Decision guards include reads and retries.** Evaluate the current attempt mode
  and current decision scope before reporting authority. A saved recovery claim
  must not bypass a later decision after recovery changes into ordinary work.
  Keep checkpoint, inspection, and release available while work is blocked.
- **Append the revision before advancing its projection.** Historical knowledge
  reimports must satisfy the same immutable-revision trigger as ordinary edits.
  Keep the standard scope/provenance shape and immutable record kind so imported
  records remain searchable and correctable through the same interface.
- **Bind SQL parameters consistently.** Mixing reused numbered parameters with
  unnamed placeholders can shift SQLx's argument mapping. Use a consistent scheme
  and exercise create/detail/list/next-selection together after projection changes.
