//! Reading a worker-written file without following anything the worker
//! planted, and creating the directories a launch may reach.
//!
//! A file is opened once with its final reparse point unresolved. Every
//! check then uses that handle, so a rename or junction swap after the open
//! cannot change what is read, and a link in a parent directory is caught
//! because the handle's real path leaves the launch root. Creation and
//! deletion inside worker-writable trees need handle-relative operations,
//! which this module does not provide.
use super::sys::{Descriptor, Handle, check};
use super::text::wide;
use anyhow::{Result, bail, ensure};
use std::path::Path;
use windows_sys::Win32::Foundation::GENERIC_READ;
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, CreateDirectoryW, CreateFileW, FILE_ATTRIBUTE_DIRECTORY,
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, GetFileInformationByHandle,
    GetFinalPathNameByHandleW, OPEN_EXISTING, ReadFile,
};

/// The longest real path the reader accepts, in UTF-16 units.
const MAX_PATH_UNITS: usize = 1024;

/// A worker-written path was not a plain single-link file inside its root.
#[derive(Debug)]
pub struct PathRefused(pub String);

impl std::fmt::Display for PathRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the path is refused: {}", self.0)
    }
}

impl std::error::Error for PathRefused {}

/// Refuses with `reason`.
fn refuse<T>(reason: impl Into<String>) -> Result<T> {
    Err(PathRefused(reason.into()).into())
}

/// Creates the directory `path` with exactly the access list `sddl`
/// describes, so it never exists with what its parent would have granted.
pub fn create_directory(path: &Path, sddl: &str) -> Result<()> {
    let (name, descriptor) = (wide(&path.display().to_string())?, Descriptor::parse(sddl)?);
    let attributes = descriptor.attributes(false);
    // SAFETY: the name is NUL-terminated; `attributes` and the descriptor
    // it borrows outlive the call.
    let created = unsafe { CreateDirectoryW(name.as_ptr(), &attributes) };
    check(created, &format!("creating {}", path.display()))
}

/// Reads `relative` below `root` by handle: at most `limit` bytes of a
/// regular, single-link file whose real path is inside the real `root`.
pub fn validated_read(root: &Path, relative: &Path, limit: usize) -> Result<Vec<u8>> {
    let file = open_unresolved(&root.join(relative))?;
    refuse_links(&file, false)?;
    let directory = open_unresolved(root)?;
    refuse_links(&directory, true)?;
    let prefix = final_path(&directory)? + "\\";
    if !final_path(&file)?.starts_with(&prefix) {
        return refuse("outside the launch root");
    }
    read(&file, limit)
}

/// Opens `path` for reading without resolving a final reparse point.
fn open_unresolved(path: &Path) -> Result<Handle> {
    let name = wide(&path.display().to_string())?;
    let share = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;
    let flags = FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS;
    // SAFETY: the name is NUL-terminated; no attributes and no template.
    let raw = unsafe {
        let (none, template) = (std::ptr::null(), std::ptr::null_mut());
        CreateFileW(
            name.as_ptr(),
            GENERIC_READ,
            share,
            none,
            OPEN_EXISTING,
            flags,
            template,
        )
    };
    Handle::owned(raw, "opening a worker path").or_else(|error| refuse(error.to_string()))
}

/// Refuses a reparse point, the wrong kind of object and a file with more
/// than one name.
fn refuse_links(handle: &Handle, directory: bool) -> Result<()> {
    // SAFETY: an all-zero BY_HANDLE_FILE_INFORMATION is valid to overwrite.
    let mut information: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: the handle is live and `information` is writable.
    let read = unsafe { GetFileInformationByHandle(handle.raw(), &mut information) };
    check(read, "GetFileInformationByHandle")?;
    let attributes = information.dwFileAttributes;
    if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return refuse("reparse point");
    }
    if (attributes & FILE_ATTRIBUTE_DIRECTORY != 0) != directory {
        return refuse("unexpected object kind");
    }
    if !directory && information.nNumberOfLinks != 1 {
        return refuse(format!("{} hard links", information.nNumberOfLinks));
    }
    Ok(())
}

/// The real path of an open handle as Windows resolves it, case-folded.
fn final_path(handle: &Handle) -> Result<String> {
    let mut buffer = vec![0_u16; MAX_PATH_UNITS];
    // SAFETY: the handle is live and the buffer is writable for its length.
    let length = unsafe {
        GetFinalPathNameByHandleW(handle.raw(), buffer.as_mut_ptr(), buffer.len() as u32, 0)
    } as usize;
    if length == 0 || length >= buffer.len() {
        return refuse("final path unavailable");
    }
    match String::from_utf16(&buffer[..length]) {
        Ok(path) => Ok(path.to_lowercase()),
        Err(_) => refuse("final path is not UTF-16"),
    }
}

/// Reads the whole file through the validated handle, refusing more than
/// `limit` bytes.
fn read(handle: &Handle, limit: usize) -> Result<Vec<u8>> {
    ensure!(limit < u32::MAX as usize, "the read limit is too large");
    let mut bytes = vec![0_u8; limit + 1];
    let mut filled = 0;
    while filled <= limit {
        let (rest, mut count) = (&mut bytes[filled..], 0_u32);
        // SAFETY: `rest` is writable for its length; no overlapped I/O.
        let read = unsafe {
            let none = std::ptr::null_mut();
            ReadFile(
                handle.raw(),
                rest.as_mut_ptr(),
                rest.len() as u32,
                &mut count,
                none,
            )
        };
        check(read, "ReadFile")?;
        if count == 0 {
            bytes.truncate(filled);
            return Ok(bytes);
        }
        filled += count as usize;
    }
    bail!(PathRefused("larger than the read limit".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Whether reading `relative` below `root` was refused as a path.
    fn refused(root: &Path, relative: &str) -> bool {
        match validated_read(root, Path::new(relative), 64) {
            Err(error) => error.downcast_ref::<PathRefused>().is_some(),
            Ok(_) => false,
        }
    }

    #[test]
    fn a_plain_file_is_read_within_its_limit() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("plain.txt"), b"plain").unwrap();
        let read = validated_read(dir.path(), Path::new("plain.txt"), 64).unwrap();
        assert_eq!(read, b"plain");
        assert_eq!(
            validated_read(dir.path(), Path::new("plain.txt"), 5).unwrap(),
            b"plain"
        );
        let over = validated_read(dir.path(), Path::new("plain.txt"), 4).unwrap_err();
        assert!(over.downcast_ref::<PathRefused>().is_some());
    }

    #[test]
    fn hard_links_directories_and_missing_files_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"x").unwrap();
        fs::hard_link(dir.path().join("a.txt"), dir.path().join("b.txt")).unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();
        for name in ["a.txt", "b.txt", "sub", "missing.txt"] {
            assert!(refused(dir.path(), name), "{name}");
        }
    }

    #[test]
    fn a_file_reached_through_a_parent_outside_the_root_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let (root, other) = (dir.path().join("root"), dir.path().join("other"));
        fs::create_dir(&root).unwrap();
        fs::create_dir(&other).unwrap();
        fs::write(other.join("secret.txt"), b"s").unwrap();
        assert!(refused(&root, "..\\other\\secret.txt"));
    }

    #[test]
    fn a_directory_is_created_with_exactly_the_access_list_asked_for() {
        let dir = tempfile::tempdir().unwrap();
        let owner = crate::host_runtime::windows::token::current_user_sid().unwrap();
        let package = "S-1-15-2-1-2-3-4-5-6-7";
        let sddl = crate::host_runtime::windows::sddl::work_directory(&owner, package).unwrap();
        let made = dir.path().join("work");
        create_directory(&made, &sddl).unwrap();
        fs::write(made.join("inside.txt"), b"x").unwrap();
        assert!(create_directory(&made, &sddl).is_err(), "never adopts one");
    }
}
