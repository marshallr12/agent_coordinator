//! An access list as data: whether it is protected from inheritance and the
//! set of its entries. Two lists are compared entry by entry, never as SDDL
//! text, because Windows spells the same list in more than one way and
//! keeps entries in an order of its own.
use crate::service::pipe_policy::checked_sid;
use anyhow::{Result, ensure};
use serde::Serialize;
use std::collections::BTreeSet;

/// Full control of a file or directory.
pub const FULL: u32 = 0x001f_01ff;
/// Modify, as `icacls` grants it for `M`.
pub const MODIFY: u32 = 0x0013_01bf;
/// Read and execute, as `icacls` grants it for `RX`.
pub const READ_EXECUTE: u32 = 0x0012_00a9;
/// Read, as `icacls` grants it for `R`.
pub const READ: u32 = 0x0012_0089;

/// The local system.
pub const SYSTEM: &str = "S-1-5-18";
/// The local Administrators group.
pub const ADMINISTRATORS: &str = "S-1-5-32-544";
/// The local Users group, which every service account belongs to.
pub const USERS: &str = "S-1-5-32-545";

/// The entry applies to files below a directory.
pub const OBJECT_INHERIT: u8 = 0x1;
/// The entry applies to directories below a directory.
pub const CONTAINER_INHERIT: u8 = 0x2;
/// The four inheritance bits of an entry's flags; the rest say how the
/// entry got there.
pub const INHERITANCE_BITS: u8 = 0xf;

/// What an entry does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// Grants the mask.
    Allow,
    /// Denies the mask.
    Deny,
    /// Any other entry type, by its number. The installer never wants one,
    /// so one that is present always counts as a difference.
    Other(u8),
}

/// One entry of an access list.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Entry {
    /// What the entry does.
    pub kind: Kind,
    /// Whom it names, as SID text.
    pub sid: String,
    /// The rights it grants or denies.
    pub mask: u32,
    /// Its inheritance bits ([`INHERITANCE_BITS`]).
    pub inheritance: u8,
    /// Whether it came from a parent rather than being set on the object.
    pub inherited: bool,
}

/// An object's discretionary access list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AccessList {
    /// Whether the list ignores what the parent would hand down.
    pub protected: bool,
    /// The entries, as a set: their order carries no meaning here.
    pub entries: BTreeSet<Entry>,
}

impl AccessList {
    /// A protected list for a directory: each `(sid, mask)` is granted and
    /// handed down to everything below.
    pub fn directory(grants: &[(&str, u32)]) -> Self {
        Self::granting(grants, OBJECT_INHERIT | CONTAINER_INHERIT)
    }

    /// A protected list for one file: each `(sid, mask)` is granted.
    pub fn file(grants: &[(&str, u32)]) -> Self {
        Self::granting(grants, 0)
    }

    /// A protected list of grants that all carry `inheritance`.
    fn granting(grants: &[(&str, u32)], inheritance: u8) -> Self {
        let entry = |&(sid, mask): &(&str, u32)| Entry {
            kind: Kind::Allow,
            sid: sid.to_owned(),
            mask,
            inheritance,
            inherited: false,
        };
        Self {
            protected: true,
            entries: grants.iter().map(entry).collect(),
        }
    }

    /// The list as SDDL text, for creating or securing an object. Only a
    /// protected list of grants can be written: that is all the installer
    /// ever sets.
    pub fn sddl(&self) -> Result<String> {
        ensure!(self.protected, "only a protected access list is ever set");
        let mut text = String::from("D:P");
        for entry in &self.entries {
            text.push_str(&entry_sddl(entry)?);
        }
        Ok(text)
    }

    /// The SDDL for creating an object with this list, owned by the
    /// Administrators group. Naming the owner makes it the same whichever
    /// administrator installs, so a later run recognises its own work.
    pub fn creation_sddl(&self) -> Result<String> {
        Ok(format!("O:BA{}", self.sddl()?))
    }

    /// The list in one line for a report: SDDL when it can be written that
    /// way, and a plain description of the entries when it cannot.
    pub fn describe(&self) -> String {
        self.sddl()
            .unwrap_or_else(|_| format!("{} {:?}", self.protection(), self.entries))
    }

    /// `protected` or `inheriting`.
    fn protection(&self) -> &'static str {
        if self.protected {
            "protected"
        } else {
            "inheriting"
        }
    }
}

/// One granting entry as SDDL. The SID is checked because it is spliced
/// into the text.
fn entry_sddl(entry: &Entry) -> Result<String> {
    ensure!(
        entry.kind == Kind::Allow && !entry.inherited,
        "only an explicit grant is ever set"
    );
    let sid = checked_sid(&entry.sid)?;
    Ok(format!(
        "(A;{};{:#x};;;{sid})",
        inheritance_sddl(entry.inheritance),
        entry.mask
    ))
}

/// The SDDL letters of the inheritance bits.
fn inheritance_sddl(bits: u8) -> String {
    let names = [(0x1, "OI"), (0x2, "CI"), (0x4, "NP"), (0x8, "IO")];
    let set = names.iter().filter(|(bit, _)| bits & bit != 0);
    set.map(|(_, name)| *name).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_directory_list_is_protected_and_handed_down() {
        let list = AccessList::directory(&[(SYSTEM, FULL), (ADMINISTRATORS, FULL)]);
        assert_eq!(
            list.sddl().unwrap(),
            "D:P(A;OICI;0x1f01ff;;;S-1-5-18)(A;OICI;0x1f01ff;;;S-1-5-32-544)"
        );
    }

    #[test]
    fn a_new_object_is_owned_by_administrators() {
        let list = AccessList::directory(&[(SYSTEM, FULL)]);
        assert_eq!(
            list.creation_sddl().unwrap(),
            "O:BAD:P(A;OICI;0x1f01ff;;;S-1-5-18)"
        );
    }

    #[test]
    fn a_file_list_hands_nothing_down() {
        let list = AccessList::file(&[(SYSTEM, FULL), ("S-1-5-80-1-2-3-4-5", READ)]);
        assert_eq!(
            list.sddl().unwrap(),
            "D:P(A;;0x1f01ff;;;S-1-5-18)(A;;0x120089;;;S-1-5-80-1-2-3-4-5)"
        );
    }

    #[test]
    fn the_order_of_entries_makes_no_difference() {
        let one = AccessList::directory(&[(SYSTEM, FULL), (ADMINISTRATORS, FULL)]);
        let other = AccessList::directory(&[(ADMINISTRATORS, FULL), (SYSTEM, FULL)]);
        assert_eq!(one, other);
    }

    #[test]
    fn text_that_is_not_a_sid_never_reaches_sddl() {
        let forged = AccessList::directory(&[("S-1-5-32)(A;;GA;;;WD", FULL)]);
        assert!(forged.sddl().is_err());
        assert!(forged.describe().contains("protected"));
    }

    #[test]
    fn a_list_that_inherits_or_denies_is_described_but_not_written() {
        let mut list = AccessList::file(&[(SYSTEM, FULL)]);
        list.protected = false;
        assert!(list.sddl().is_err());
        assert!(list.describe().starts_with("inheriting"));
    }
}
