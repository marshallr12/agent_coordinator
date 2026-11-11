//! The window station a launcher makes and the private desktop each launch
//! gets inside it.
//!
//! Windows gives an account one unnamed window station per logon session,
//! and only an administrator may create a named one. A launcher therefore
//! makes that one station and lists, in its access list, only its own
//! account and the packages of its live launches. If the station already
//! exists, the launcher takes it over only when Windows confirms that it
//! was made for this process alone ([`super::adoption`]), which is how a
//! service finds its station; any other existing station is controlled by
//! whoever made it, so the launcher refuses ([`StationNotOurs`]). Launches
//! of one launcher share the station's clipboard and atom table; their
//! desktops are separate.
use super::sys::{Descriptor, check, last_error};
use super::text::wide;
use super::{adoption, sddl, token};
use anyhow::{Context, Result};
use std::ffi::c_void;
use std::sync::Mutex;
use windows_sys::Win32::Foundation::{ERROR_ALREADY_EXISTS, GENERIC_ALL, GetLastError, HANDLE};
use windows_sys::Win32::Security::{
    DACL_SECURITY_INFORMATION, LABEL_SECURITY_INFORMATION, SetUserObjectSecurity,
};
use windows_sys::Win32::System::StationsAndDesktops::{
    CloseDesktop, CloseWindowStation, CreateDesktopW, CreateWindowStationW,
    GetProcessWindowStation, GetUserObjectInformationW, HDESK, HWINSTA, SetProcessWindowStation,
    UOI_NAME,
};
use windows_sys::Win32::UI::WindowsAndMessaging::CWF_CREATE_ONLY;

/// Held while this process's own window station is switched, which is
/// process-wide state.
static STATION_SWITCH: Mutex<()> = Mutex::new(());

/// The logon session's window station already existed and the launcher
/// could not show that it controls who may open it. The text says which
/// fact was missing.
#[derive(Debug)]
pub struct StationNotOurs(pub String);

impl std::fmt::Display for StationNotOurs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the logon session's window station was made by another program ({}); \
             the launcher starts nothing in a station it does not control",
            self.0
        )
    }
}

impl std::error::Error for StationNotOurs {}

/// The name Windows reports for a station or desktop handle.
fn object_name(handle: HANDLE) -> Result<String> {
    let mut buffer = [0_u16; 256];
    let (pointer, size) = (
        buffer.as_mut_ptr().cast::<c_void>(),
        size_of_val(&buffer) as u32,
    );
    let mut needed = 0_u32;
    // SAFETY: `pointer` is writable for `size` bytes and `needed` is live.
    let read = unsafe { GetUserObjectInformationW(handle, UOI_NAME, pointer, size, &mut needed) };
    check(read, "GetUserObjectInformationW")?;
    let length = buffer.iter().position(|&unit| unit == 0).unwrap_or(0);
    String::from_utf16(&buffer[..length]).context("an object name is not UTF-16")
}

/// The window station one launcher made for its launches.
#[derive(Debug)]
pub struct Station {
    handle: HWINSTA,
    owner: String,
    packages: Vec<String>,
    name: String,
}

// SAFETY: a window station handle is a process-wide value.
unsafe impl Send for Station {}

impl Station {
    /// Makes the logon session's window station, open to `owner` alone. A
    /// station that already exists is taken over if it was made for this
    /// process alone ([`Station::adopt`]) and refused with
    /// [`StationNotOurs`] otherwise.
    pub fn create(owner: &str) -> Result<Self> {
        let descriptor = Descriptor::parse(&sddl::station(owner, &[])?)?;
        let attributes = descriptor.attributes(false);
        // SAFETY: an unnamed station; `attributes` and the descriptor it
        // borrows outlive the call.
        let handle = unsafe {
            CreateWindowStationW(std::ptr::null(), CWF_CREATE_ONLY, GENERIC_ALL, &attributes)
        };
        if !handle.is_null() {
            return Self::holding(handle, owner);
        }
        // SAFETY: GetLastError reads thread-local state.
        if unsafe { GetLastError() } != ERROR_ALREADY_EXISTS {
            return Err(last_error("CreateWindowStationW"));
        }
        Self::adopt(owner)
    }

    /// The station behind `handle`, which is closed again if its name
    /// cannot be read.
    fn holding(handle: HWINSTA, owner: &str) -> Result<Self> {
        let mut station = Self {
            handle,
            owner: owner.to_owned(),
            packages: Vec::new(),
            name: String::new(),
        };
        station.name = object_name(handle)?;
        Ok(station)
    }

    /// Takes over the station Windows made for this logon session before
    /// the launcher ran. Every fact [`adoption`] names is checked first;
    /// then the access list is replaced by the launcher's own, with the
    /// label a launch needs, and read back. Nothing is changed in a
    /// station that fails a check.
    fn adopt(owner: &str) -> Result<Self> {
        let station = Self::holding(open_existing()?, owner)?;
        let session = token::current_logon_session()?;
        // SAFETY: returns this process's current station; it is not closed.
        let own = object_name(unsafe { GetProcessWindowStation() })?;
        adoption::is_own(&station.name, &own, session)?;
        adoption::owner_is_trusted(&adoption::owner(station.handle)?, owner)?;
        adoption::is_alone(session)?;
        station.apply(DACL_SECURITY_INFORMATION | LABEL_SECURITY_INFORMATION)?;
        station.confirm()?;
        Ok(station)
    }

    /// Refuses unless Windows reports the access list the launcher wrote:
    /// the owner and each admitted package, and nobody else.
    fn confirm(&self) -> Result<()> {
        let mut expected = vec![self.owner.clone()];
        expected.extend(self.packages.iter().cloned());
        let listed = adoption::allowed(self.handle)?;
        if listed != expected {
            let reason = format!("its access list reads back as {listed:?}");
            return Err(StationNotOurs(reason).into());
        }
        Ok(())
    }

    /// The station's name, the first half of a launch's desktop name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Rewrites the parts of the station's security named in `requested`:
    /// the access list is the owner and each admitted package.
    fn apply(&self, requested: u32) -> Result<()> {
        let descriptor = Descriptor::parse(&sddl::station(&self.owner, &self.packages)?)?;
        // SAFETY: the station handle is live; `requested` and the
        // descriptor outlive the call.
        let set = unsafe { SetUserObjectSecurity(self.handle, &requested, descriptor.pointer()) };
        check(set, "SetUserObjectSecurity on the window station")
    }

    /// Lets one launch's package open the station.
    pub fn admit(&mut self, package: &str) -> Result<()> {
        self.packages.push(package.to_owned());
        self.apply(DACL_SECURITY_INFORMATION)
    }

    /// Withdraws a finished launch's package.
    pub fn dismiss(&mut self, package: &str) -> Result<()> {
        self.packages.retain(|admitted| admitted != package);
        self.apply(DACL_SECURITY_INFORMATION)
    }
}

/// Opens the logon session's existing station with every right, which
/// rewriting its access list and label needs. A station Windows will not
/// open that way is not the launcher's to control.
fn open_existing() -> Result<HWINSTA> {
    // SAFETY: an unnamed station, no flags and no attributes: an existing
    // station is opened as it is.
    let handle =
        unsafe { CreateWindowStationW(std::ptr::null(), 0, GENERIC_ALL, std::ptr::null()) };
    if handle.is_null() {
        // SAFETY: GetLastError reads thread-local state.
        let code = unsafe { GetLastError() };
        let reason = format!("Windows refused full access to it with error {code}");
        return Err(StationNotOurs(reason).into());
    }
    Ok(handle)
}

impl Drop for Station {
    fn drop(&mut self) {
        // SAFETY: the handle is owned here and closed once; Windows
        // destroys the station with its last handle.
        unsafe { CloseWindowStation(self.handle) };
    }
}

/// One launch's private desktop: only the launcher's account and that
/// launch's package may open it.
#[derive(Debug)]
pub struct Desktop {
    handle: HDESK,
    name: String,
}

// SAFETY: a desktop handle is a process-wide value.
unsafe impl Send for Desktop {}

impl Desktop {
    /// Admits `package` to `station` and makes the desktop `label` in it.
    pub fn create(station: &mut Station, package: &str, label: &str) -> Result<Self> {
        let descriptor = Descriptor::parse(&sddl::desktop(&station.owner, package)?)?;
        station.admit(package)?;
        let handle = match create_in(station, label, &descriptor) {
            Ok(handle) => handle,
            Err(error) => {
                let _ = station.dismiss(package);
                return Err(error);
            }
        };
        let name = object_name(handle).map(|own| format!("{}\\{own}", station.name));
        let desktop = Self {
            handle,
            name: name.as_ref().cloned().unwrap_or_default(),
        };
        name.map(|_| desktop)
    }

    /// `station\desktop`, the name process creation takes.
    pub fn name(&self) -> &str {
        &self.name
    }
}

impl Drop for Desktop {
    fn drop(&mut self) {
        // SAFETY: the handle is owned here and closed exactly once.
        unsafe { CloseDesktop(self.handle) };
    }
}

/// Creates the desktop inside `station`: a desktop is made in the calling
/// process's station, so this process switches to it and back.
fn create_in(station: &Station, label: &str, descriptor: &Descriptor) -> Result<HDESK> {
    let (label, attributes) = (wide(label)?, descriptor.attributes(false));
    let _switching = STATION_SWITCH
        .lock()
        .unwrap_or_else(|held| held.into_inner());
    // SAFETY: returns this process's current station; it is not closed.
    let original = unsafe { GetProcessWindowStation() };
    // SAFETY: the station handle is live.
    let switched = unsafe { SetProcessWindowStation(station.handle) };
    check(switched, "SetProcessWindowStation")?;
    // SAFETY: the name is NUL-terminated; no device or mode; `attributes`
    // and the descriptor it borrows outlive the call.
    let handle = unsafe {
        let (none, mode) = (std::ptr::null(), std::ptr::null());
        CreateDesktopW(label.as_ptr(), none, mode, 0, GENERIC_ALL, &attributes)
    };
    let failure = handle.is_null().then(|| last_error("CreateDesktopW"));
    // SAFETY: `original` is the station this process had before.
    unsafe { SetProcessWindowStation(original) };
    failure.map_or(Ok(handle), Err)
}
