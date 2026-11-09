//! Access lists, as SDDL text, for the objects one launch may reach. Each is
//! complete: an object gets exactly these entries, never what its parent
//! happened to grant. Nothing here calls the operating system.
use anyhow::{Result, ensure};

/// Read and execute, as `icacls` grants it for `RX`.
const READ_EXECUTE: &str = "0x1200a9";
/// Modify, as `icacls` grants it for `M`.
const MODIFY: &str = "0x1301bf";
/// A low integrity label that forbids writing up. An AppContainer token runs
/// at low integrity, so anything it must write to carries this label.
const LOW_LABEL: &str = "S:(ML;;NW;;;LW)";
/// The same label, inherited by everything created below a directory.
const LOW_LABEL_INHERITED: &str = "S:(ML;OICI;NW;;;LW)";

/// `sid` if it is plain `S-1-...` SID text. A SID is spliced into SDDL, so
/// anything else could add an entry of its own.
pub fn checked(sid: &str) -> Result<&str> {
    let plain = sid.starts_with("S-1-")
        && sid.len() <= 184
        && sid[4..].split('-').all(|part| {
            !part.is_empty() && part.len() <= 10 && part.bytes().all(|b| b.is_ascii_digit())
        });
    ensure!(plain, "{sid:?} is not SID text");
    Ok(sid)
}

/// The window station of one launcher: its owner and the package of each
/// live launch may open it, and nobody else.
pub fn station(owner: &str, packages: &[String]) -> Result<String> {
    let mut entries = format!("D:(A;;GA;;;{})", checked(owner)?);
    for package in packages {
        entries.push_str(&format!("(A;;GA;;;{})", checked(package)?));
    }
    Ok(entries + LOW_LABEL)
}

/// The private desktop of one launch: its owner and its own package.
pub fn desktop(owner: &str, package: &str) -> Result<String> {
    station(owner, &[package.to_owned()])
}

/// The owner, the system and administrators in full, inherited by children.
fn trusted(owner: &str) -> Result<String> {
    let owner = checked(owner)?;
    Ok(format!(
        "D:P(A;OICI;FA;;;{owner})(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)"
    ))
}

/// A launch's control directory: the launch may read it and change nothing.
pub fn control_directory(owner: &str, package: &str) -> Result<String> {
    let package = checked(package)?;
    Ok(format!(
        "{}(A;OICI;{READ_EXECUTE};;;{package})",
        trusted(owner)?
    ))
}

/// A launch's working directory: the only tree the launch may modify.
pub fn work_directory(owner: &str, package: &str) -> Result<String> {
    let package = checked(package)?;
    Ok(format!(
        "{}(A;OICI;{MODIFY};;;{package}){LOW_LABEL_INHERITED}",
        trusted(owner)?
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const OWNER: &str = "S-1-5-21-1-2-3-1001";
    const PACKAGE: &str = "S-1-15-2-1-2-3-4-5-6-7";

    #[test]
    fn only_plain_sid_text_reaches_an_access_list() {
        assert!(checked(OWNER).is_ok() && checked(PACKAGE).is_ok());
        for forged in [
            "",
            "S-1-",
            "WD",
            "S-1-5-32)(A;;GA;;;WD",
            "S-1-5--32",
            "S-1-5-3x",
        ] {
            assert!(checked(forged).is_err(), "{forged:?}");
            assert!(station(forged, &[]).is_err(), "{forged:?}");
            assert!(desktop(OWNER, forged).is_err(), "{forged:?}");
            assert!(work_directory(OWNER, forged).is_err(), "{forged:?}");
        }
    }

    #[test]
    fn a_station_names_its_owner_and_its_live_launches_only() {
        assert_eq!(
            station(OWNER, &[]).unwrap(),
            format!("D:(A;;GA;;;{OWNER})S:(ML;;NW;;;LW)")
        );
        let admitted = station(OWNER, &[PACKAGE.to_owned()]).unwrap();
        assert_eq!(
            admitted,
            format!("D:(A;;GA;;;{OWNER})(A;;GA;;;{PACKAGE})S:(ML;;NW;;;LW)")
        );
        assert_eq!(desktop(OWNER, PACKAGE).unwrap(), admitted);
    }

    #[test]
    fn a_launch_reads_its_control_directory_and_modifies_only_its_work() {
        let control = control_directory(OWNER, PACKAGE).unwrap();
        assert!(control.starts_with("D:P(A;OICI;FA;;;S-1-5-21-1-2-3-1001)"));
        assert!(control.ends_with(&format!("(A;OICI;0x1200a9;;;{PACKAGE})")));
        assert!(
            !control.contains("S:("),
            "a read-only tree needs no low label"
        );
        let work = work_directory(OWNER, PACKAGE).unwrap();
        assert!(work.contains(&format!("(A;OICI;0x1301bf;;;{PACKAGE})")));
        assert!(work.ends_with("S:(ML;OICI;NW;;;LW)"));
    }
}
