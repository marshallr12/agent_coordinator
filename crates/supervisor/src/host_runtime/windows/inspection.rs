//! What Windows shows this process about a window station it did not make.
//!
//! A refusal to take a station over has to say what the station is, or the
//! person reading it learns only an error code. [`explained`] adds to every
//! such refusal the account and station of this process, the station's
//! owner, access list and label as SDDL text, the rights Windows grants
//! this process on it, and the other processes in the logon session.
//!
//! Everything here only reads. A station is opened by name, which never
//! makes one, with one right at a time, and closed again.
use super::desktop::{StationNotOurs, object_name};
use super::sys::descriptor_text;
use super::text::wide;
use super::{adoption, token};
use anyhow::Result;
use std::fmt;
use windows_sys::Win32::Foundation::{ERROR_INVALID_NAME, GetLastError};
use windows_sys::Win32::Security::{
    DACL_SECURITY_INFORMATION, GROUP_SECURITY_INFORMATION, LABEL_SECURITY_INFORMATION,
    OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
};
use windows_sys::Win32::System::StationsAndDesktops::{
    CloseWindowStation, GetProcessWindowStation, HWINSTA, OpenWindowStationW,
};

/// Read the security descriptor of an object.
const READ_CONTROL: u32 = 0x0002_0000;
/// Every right `GENERIC_ALL` means for a window station.
pub const EVERY_RIGHT: u32 = 0x000F_037F;
/// The rights in [`EVERY_RIGHT`], by the names Windows documents.
const RIGHTS: [(&str, u32); 13] = [
    ("WINSTA_ENUMDESKTOPS", 0x0001),
    ("WINSTA_READATTRIBUTES", 0x0002),
    ("WINSTA_ACCESSCLIPBOARD", 0x0004),
    ("WINSTA_CREATEDESKTOP", 0x0008),
    ("WINSTA_WRITEATTRIBUTES", 0x0010),
    ("WINSTA_ACCESSGLOBALATOMS", 0x0020),
    ("WINSTA_EXITWINDOWS", 0x0040),
    ("WINSTA_ENUMERATE", 0x0100),
    ("WINSTA_READSCREEN", 0x0200),
    ("DELETE", 0x0001_0000),
    ("READ_CONTROL", READ_CONTROL),
    ("WRITE_DAC", 0x0004_0000),
    ("WRITE_OWNER", 0x0008_0000),
];
/// The parts of a station's security that are reported.
const PARTS: u32 = OWNER_SECURITY_INFORMATION
    | GROUP_SECURITY_INFORMATION
    | DACL_SECURITY_INFORMATION
    | LABEL_SECURITY_INFORMATION;

/// A station handle opened only to look; dropping it closes it.
struct Opened(HWINSTA);

impl Drop for Opened {
    fn drop(&mut self) {
        // SAFETY: the handle is owned here and closed exactly once.
        unsafe { CloseWindowStation(self.0) };
    }
}

/// Opens the existing station `name` with exactly `rights`, or returns the
/// Windows error that refused it. Opening by name never makes a station.
fn open(name: &str, rights: u32) -> Result<Opened, u32> {
    let name = wide(name).map_err(|_| ERROR_INVALID_NAME)?;
    // SAFETY: the name is NUL-terminated; the handle is not inherited.
    let handle = unsafe { OpenWindowStationW(name.as_ptr(), 0, rights) };
    if handle.is_null() {
        // SAFETY: GetLastError reads thread-local state.
        return Err(unsafe { GetLastError() });
    }
    Ok(Opened(handle))
}

/// The rights of [`EVERY_RIGHT`] Windows grants this process on the station
/// `name`, each asked for by itself.
pub fn granted(name: &str) -> u32 {
    let given = RIGHTS
        .iter()
        .filter(|(_, right)| open(name, *right).is_ok());
    given.fold(0, |mask, (_, right)| mask | right)
}

/// The names of the rights of [`EVERY_RIGHT`] that `mask` lacks.
fn missing(mask: u32) -> Vec<&'static str> {
    let lacks = RIGHTS.iter().filter(|(_, right)| mask & right == 0);
    lacks.map(|(name, _)| *name).collect()
}

/// The owner, group, access list and label of `station` as SDDL text. A
/// station whose label Windows will not show is printed without it.
fn security_of(station: HWINSTA) -> Result<String> {
    let buffer = adoption::descriptor(station, PARTS)
        .or_else(|_| adoption::descriptor(station, PARTS & !LABEL_SECURITY_INFORMATION))?;
    descriptor_text(buffer.as_ptr() as PSECURITY_DESCRIPTOR, PARTS)
}

/// `outcome` as text: the value, or why Windows did not give it.
fn or_why<T: fmt::Debug>(outcome: Result<T>) -> String {
    match outcome {
        Ok(value) => format!("{value:?}"),
        Err(error) => format!("unknown ({error:#})"),
    }
}

/// The security of the station `name`. When Windows refuses to open it for
/// reading and this process runs in it (`attached`), the handle Windows
/// gave the process at start is tried instead.
fn security(name: &str, attached: bool) -> String {
    match open(name, READ_CONTROL) {
        Ok(station) => or_why(security_of(station.0)),
        Err(code) if attached => {
            // SAFETY: returns this process's current station; it is not closed.
            let own = security_of(unsafe { GetProcessWindowStation() });
            format!("{} (READ_CONTROL refused: error {code})", or_why(own))
        }
        Err(code) => format!("unknown (READ_CONTROL refused: error {code})"),
    }
}

/// What Windows shows this process about its logon session's station.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Facts {
    /// The account this process runs as.
    pub account: String,
    /// The station this process runs in.
    pub process_station: String,
    /// The name of the logon session's unnamed station.
    pub station: String,
    /// The station's owner, group, access list and label, as SDDL.
    pub security: String,
    /// The rights of [`EVERY_RIGHT`] Windows grants this process on it.
    pub granted: u32,
    /// The other processes in the logon session.
    pub companions: String,
}

impl fmt::Display for Facts {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let lacks = missing(self.granted);
        let (account, own, station) = (&self.account, &self.process_station, &self.station);
        write!(f, "Windows shows: this process is {account} in {own}; ")?;
        write!(f, "{station} has security {}; ", self.security)?;
        write!(
            f,
            "of the rights {EVERY_RIGHT:#010x} it grants this process "
        )?;
        write!(f, "{:#010x}", self.granted)?;
        if !lacks.is_empty() {
            write!(f, " (not {})", lacks.join(", "))?;
        }
        let others = &self.companions;
        write!(f, "; other processes in the logon session: {others}")
    }
}

/// Reads [`Facts`] about this process's logon session's station.
pub fn facts() -> Result<Facts> {
    let session = token::current_logon_session()?;
    let station = adoption::station_name(session);
    // SAFETY: returns this process's current station; it is not closed.
    let process_station = object_name(unsafe { GetProcessWindowStation() })?;
    let attached = process_station.eq_ignore_ascii_case(&station);
    Ok(Facts {
        account: token::current_user_sid()?,
        security: security(&station, attached),
        granted: granted(&station),
        companions: or_why(adoption::companions(session)),
        process_station,
        station,
    })
}

/// `error` with what Windows shows about the station added to its reason,
/// when it is a refusal to take the station over. Any other error is
/// returned as it is.
pub fn explained(error: anyhow::Error) -> anyhow::Error {
    match error.downcast::<StationNotOurs>() {
        Ok(StationNotOurs(reason)) => {
            let shown = facts().map_or_else(
                |error| format!("what Windows shows could not be read: {error:#}"),
                |facts| facts.to_string(),
            );
            StationNotOurs(format!("{reason}; {shown}")).into()
        }
        Err(other) => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Facts as a real service saw them, with `granted` rights.
    fn seen(granted: u32) -> Facts {
        Facts {
            account: "S-1-5-80-1-2-3-4-5".into(),
            process_station: "Service-0x0-51f3$".into(),
            station: "Service-0x0-51f3$".into(),
            security: "\"O:BAG:SYD:(A;;0xf037f;;;BA)\"".into(),
            granted,
            companions: "[]".into(),
        }
    }

    #[test]
    fn the_text_names_the_account_the_security_and_each_right_not_granted() {
        let text = seen(EVERY_RIGHT & !0x000C_0000).to_string();
        assert!(text.starts_with("Windows shows: this process is S-1-5-80-1-2-3-4-5 in"));
        assert!(text.contains("has security \"O:BAG:SYD:(A;;0xf037f;;;BA)\""));
        assert!(text.contains("0x000f037f it grants this process 0x0003037f"));
        assert!(text.contains("(not WRITE_DAC, WRITE_OWNER)"), "{text}");
        assert!(text.ends_with("other processes in the logon session: []"));
    }

    #[test]
    fn a_station_that_grants_every_right_names_none_as_missing() {
        assert!(missing(EVERY_RIGHT).is_empty());
        assert!(!seen(EVERY_RIGHT).to_string().contains("(not "));
        assert_eq!(missing(0).len(), RIGHTS.len());
        let all = RIGHTS.iter().fold(0, |mask, (_, right)| mask | right);
        assert_eq!(all, EVERY_RIGHT, "the names cover every right");
    }

    #[test]
    fn a_station_that_does_not_exist_grants_nothing_and_is_not_made() {
        let name = "agentc-no-such-station";
        assert_eq!(granted(name), 0);
        assert!(security(name, false).starts_with("unknown (READ_CONTROL refused"));
        assert!(open(name, READ_CONTROL).is_err(), "looking made no station");
    }

    #[test]
    fn this_process_can_read_what_windows_shows_about_its_session() {
        let facts = facts().unwrap();
        eprintln!("{facts}");
        assert!(facts.account.starts_with("S-1-5-"), "{facts}");
        assert!(facts.station.starts_with("Service-0x"), "{facts}");
        assert!(!facts.process_station.is_empty(), "{facts}");
    }

    #[test]
    fn only_a_refusal_to_take_over_is_explained() {
        let refusal = explained(StationNotOurs("its owner is X".into()).into());
        let text = refusal.downcast_ref::<StationNotOurs>().unwrap().0.clone();
        assert!(text.starts_with("its owner is X; "), "{text}");
        assert!(text.contains("Windows shows"), "{text}");
        let other = explained(anyhow::anyhow!("something else"));
        assert_eq!(other.to_string(), "something else");
    }
}
