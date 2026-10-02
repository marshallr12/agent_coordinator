//! The modules both binaries of this crate share: `agentc-integrator` (the
//! deterministic integrator) and `agentc-push` (the candidate-push helper)
//! read their GitHub App settings, sign App JWTs, mint installation tokens and
//! answer Git credential prompts through them.
pub mod askpass;
pub mod config;
pub mod github;
