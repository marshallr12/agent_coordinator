//! The modules both binaries of this crate share: `agentc-integrator` (the
//! deterministic integrator) and `agentc-push` (the candidate-push helper)
//! read their GitHub App settings, sign App JWTs, mint installation tokens and
//! answer Git credential prompts through them. The Windows publication broker
//! (in `agentc-supervisor`) mints, budgets and answers prompts the same way
//! as `agentc-push` ([`mint_budget`], [`push_askpass`], [`push_preamble`]).
pub mod askpass;
pub mod config;
pub mod github;
pub mod mint_budget;
pub mod push_askpass;
pub mod push_preamble;
