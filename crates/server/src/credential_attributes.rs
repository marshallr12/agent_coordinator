//! Agent credential attributes: `class` (who drives the credential) and
//! `access` (whether it may change coordination state).
//!
//! `class=supervised` marks credentials held by unattended, supervisor-launched
//! agents; every event records the class so autonomy can be measured from the
//! audit log. `access=read` credentials (reviewer and shadow host principals)
//! may inspect state and manage their own session, but every other mutation is
//! refused before it touches coordination state.
use serde::{Deserialize, Serialize};

use crate::{auth::Actor, error::AppError};

/// Who drives a credential: a human-attended harness or the supervisor.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialClass {
    #[default]
    Interactive,
    Supervised,
}

/// Whether a credential may change coordination state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialAccess {
    #[default]
    Write,
    Read,
}

/// The attributes stored on one agent credential.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct CredentialAttributes {
    #[serde(default)]
    pub class: CredentialClass,
    #[serde(default)]
    pub access: CredentialAccess,
}

impl CredentialClass {
    /// True for the default, `interactive`.
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// The database and wire spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Interactive => "interactive",
            Self::Supervised => "supervised",
        }
    }
}

impl CredentialAccess {
    /// True for the default, `write`.
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// The database and wire spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Write => "write",
            Self::Read => "read",
        }
    }
}

impl CredentialAttributes {
    /// Decodes the stored columns; unknown values fail closed as read-only
    /// interactive so a corrupted row can never gain write authority.
    pub fn from_columns(class: &str, access: &str) -> Self {
        let class = match class {
            "supervised" => CredentialClass::Supervised,
            _ => CredentialClass::Interactive,
        };
        let access = match access {
            "write" => CredentialAccess::Write,
            _ => CredentialAccess::Read,
        };
        Self { class, access }
    }
}

/// Operations a read-only credential may perform: managing its own session.
fn allowed_for_read_access(operation: &str) -> bool {
    operation == "POST /api/v1/sessions"
        || (operation.starts_with("POST /api/v1/sessions/")
            && (operation.ends_with("/close")
                || operation.ends_with("/instruction-acknowledgments")))
}

/// Refuses a mutation from a read-only credential unless it only manages the
/// caller's own session. Browser sessions and reporters carry no attributes.
pub fn require_write_access(actor: &Actor, operation: &str) -> Result<(), AppError> {
    let read_only = actor
        .credential_attributes
        .is_some_and(|a| a.access == CredentialAccess::Read);
    if read_only && !allowed_for_read_access(operation) {
        return Err(AppError::forbidden(
            "This credential is read-only; it may inspect state and manage its own session only.",
        ));
    }
    Ok(())
}

/// The class recorded on audit events; `None` for human browser sessions and
/// job reporters, which are not agent credentials.
pub fn event_class(actor: &Actor) -> Option<&'static str> {
    actor.credential_attributes.map(|a| a.class.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_columns_fail_closed() {
        let parsed = CredentialAttributes::from_columns("bogus", "bogus");
        assert_eq!(parsed.class, CredentialClass::Interactive);
        assert_eq!(parsed.access, CredentialAccess::Read);
    }

    #[test]
    fn read_access_allows_only_own_session_management() {
        assert!(allowed_for_read_access("POST /api/v1/sessions"));
        assert!(allowed_for_read_access("POST /api/v1/sessions/s1/close"));
        assert!(allowed_for_read_access(
            "POST /api/v1/sessions/s1/instruction-acknowledgments"
        ));
        assert!(!allowed_for_read_access("POST /api/v1/projects/p/tasks"));
        assert!(!allowed_for_read_access("POST /api/v1/sessions/s1/other"));
    }
}
