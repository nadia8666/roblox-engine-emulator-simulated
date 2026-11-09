//! A file's size and modification time without opening it, for caches that check a file is
//! unchanged (compiled module sources, converted bundles). `std::fs::metadata` opens a handle on
//! Windows, which with a virus scanner watching costs a hundred times a plain attribute query
//! (a recording's 1146 module checks took ~100 ms); `GetFileAttributesExW` reads the same
//! attributes from the path.

use std::path::Path;

/// What a cache compares: size, modification time (100 ns ticks on Windows, nanoseconds since
/// the Unix epoch elsewhere) and whether the path is a directory.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Stamp {
    pub len: u64,
    pub modified: u64,
    pub directory: bool,
}

#[cfg(windows)]
pub fn stamp(path: &Path) -> Option<Stamp> {
    use std::os::windows::ffi::OsStrExt;
    #[repr(C)]
    struct Data {
        attributes: u32,
        created: [u32; 2],
        accessed: [u32; 2],
        written: [u32; 2],
        size_high: u32,
        size_low: u32,
    }
    unsafe extern "system" {
        fn GetFileAttributesExW(name: *const u16, level: i32, data: *mut Data) -> i32;
    }
    const DIRECTORY: u32 = 0x10;
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    if wide.len() > 240 {
        // (past MAX_PATH the plain call fails; std adds the \\?\ prefix)
        let metadata = std::fs::metadata(path).ok()?;
        let since_1601 = metadata.modified().ok().and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |time| (time.as_nanos() / 100) as u64 + 116_444_736_000_000_000);
        return Some(Stamp { len: metadata.len(), modified: since_1601, directory: metadata.is_dir() });
    }
    let mut data = Data { attributes: 0, created: [0; 2], accessed: [0; 2], written: [0; 2], size_high: 0, size_low: 0 };
    // GetFileExInfoStandard
    if unsafe { GetFileAttributesExW(wide.as_ptr(), 0, &mut data) } == 0 {
        return None;
    }
    Some(Stamp {
        len: (u64::from(data.size_high) << 32) | u64::from(data.size_low),
        modified: (u64::from(data.written[1]) << 32) | u64::from(data.written[0]),
        directory: data.attributes & DIRECTORY != 0,
    })
}

#[cfg(not(windows))]
pub fn stamp(path: &Path) -> Option<Stamp> {
    let metadata = std::fs::metadata(path).ok()?;
    let modified = metadata.modified().ok().and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |time| time.as_nanos() as u64);
    Some(Stamp { len: metadata.len(), modified, directory: metadata.is_dir() })
}

/// The stamp of a directory listing's entry (no query of its own on Windows: the listing holds
/// it), as [`stamp`] gives it; None for a link, whose own stamp is not its target's.
pub fn entry_stamp(entry: &std::fs::DirEntry) -> Option<Stamp> {
    let kind = entry.file_type().ok()?;
    if kind.is_symlink() {
        return None;
    }
    let metadata = entry.metadata().ok()?;
    let since_epoch = metadata.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?;
    #[cfg(windows)]
    let modified = (since_epoch.as_nanos() / 100) as u64 + 116_444_736_000_000_000;
    #[cfg(not(windows))]
    let modified = since_epoch.as_nanos() as u64;
    Some(Stamp { len: metadata.len(), modified, directory: metadata.is_dir() })
}

/// Whether `path` is a file (a stamp that is not a directory's).
pub fn is_file(path: &Path) -> bool {
    stamp(path).is_some_and(|stamp| !stamp.directory)
}

/// Whether two existing paths identify the same file, including links and differently spelled paths. A path that cannot
/// be read has no identity, so it is never the same file as anything.
#[cfg(feature = "bundles")]
pub(crate) fn same_file(left: &Path, right: &Path) -> bool {
    match (file_id(left), file_id(right)) {
        (Some(left), Some(right)) => left == right,
        _ => false,
    }
}

/// The device and inode that name the file `path` ends at (links followed).
#[cfg(all(feature = "bundles", unix))]
fn file_id(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let metadata = std::fs::metadata(path).ok()?;
    Some((metadata.dev(), metadata.ino()))
}

/// The volume and file index that name the file `path` ends at (links followed).
#[cfg(all(feature = "bundles", windows))]
fn file_id(path: &Path) -> Option<(u64, u64)> {
    use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};
    /// Windows' BY_HANDLE_FILE_INFORMATION; only the volume and the index are read.
    #[repr(C)]
    #[derive(Default)]
    struct Information {
        attributes: u32,
        created: [u32; 2],
        accessed: [u32; 2],
        written: [u32; 2],
        volume: u32,
        size: [u32; 2],
        links: u32,
        index_high: u32,
        index_low: u32,
    }
    unsafe extern "system" {
        fn GetFileInformationByHandle(file: *mut std::ffi::c_void, information: *mut Information) -> i32;
    }
    const NO_ACCESS: u32 = 0;
    const SHARE_READ_WRITE_DELETE: u32 = 0b111;
    // Query identity without requesting data access or preventing a worker from keeping its file open.
    let file = std::fs::OpenOptions::new().access_mode(NO_ACCESS).share_mode(SHARE_READ_WRITE_DELETE).open(path).ok()?;
    let mut information = Information::default();
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut information) } == 0 {
        return None;
    }
    let index = (u64::from(information.index_high) << 32) | u64::from(information.index_low);
    Some((u64::from(information.volume), index))
}

/// Without an inode or a file index, the canonical path is the best identity there is.
#[cfg(all(feature = "bundles", not(any(unix, windows))))]
fn file_id(path: &Path) -> Option<std::path::PathBuf> {
    std::fs::canonicalize(path).ok()
}

/// What a thread's engine read from disk while it was noted (record/prepare.rs runs a recording up to
/// its harness's checkpoint ahead of the job and checks, when the job comes, that what it read then
/// is as it is now): each path's stamp when it was read (None: it was missing), each listing's names,
/// and why a read cannot be checked this way (an image, a failed fetch).
#[derive(Default)]
pub struct Noted {
    files: Vec<(std::path::PathBuf, Option<Stamp>)>,
    listings: Vec<(std::path::PathBuf, Option<Vec<std::ffi::OsString>>)>,
    unchecked: Option<String>,
}

thread_local! {
    static NOTING: std::cell::RefCell<Option<Noted>> = const { std::cell::RefCell::new(None) };
    /// The first read since [`forget_unchecked`] that a prepared run could not check, noted or not
    static UNCHECKED: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// Why this thread's engine read something a prepared run could not check since [`forget_unchecked`]
/// (a run that did so before its checkpoint cannot be prepared: record/prepare.rs would fail alike).
pub fn unchecked() -> Option<String> {
    UNCHECKED.with(|unchecked| unchecked.try_borrow().ok().and_then(|why| why.clone()))
}

/// Starts [`unchecked`] over (a `ree serve` worker, before each job).
pub fn forget_unchecked() {
    UNCHECKED.with(|unchecked| {
        if let Ok(mut why) = unchecked.try_borrow_mut() {
            *why = None;
        }
    });
}

/// Notes this thread's reads ([`note`], [`note_listing`], [`note_unchecked`]) until [`stop_noting`].
pub fn start_noting() {
    NOTING.with(|noting| *noting.borrow_mut() = Some(Noted::default()));
}

/// What this thread read since [`start_noting`] (each path once: a path read twice with two stamps
/// changed while it was read, which cannot be checked).
pub fn stop_noting() -> Noted {
    let mut noted = NOTING.with(|noting| noting.borrow_mut().take()).unwrap_or_default();
    noted.files.sort_by(|a, b| a.0.cmp(&b.0));
    if noted.files.windows(2).any(|pair| pair[0].0 == pair[1].0 && pair[0].1 != pair[1].1) && noted.unchecked.is_none() {
        noted.unchecked = Some("a file changed while it was read".into());
    }
    noted.files.dedup_by(|a, b| a.0 == b.0);
    noted.listings.sort_by(|a, b| a.0.cmp(&b.0));
    noted.listings.dedup_by(|a, b| a.0 == b.0);
    noted
}

fn noting(f: impl FnOnce(&mut Noted)) {
    NOTING.with(|noting| {
        if let Ok(mut noting) = noting.try_borrow_mut() {
            if let Some(noted) = noting.as_mut() {
                f(noted);
            }
        }
    });
}

fn is_noting() -> bool {
    NOTING.with(|noting| noting.try_borrow().is_ok_and(|noting| noting.is_some()))
}

/// Notes that this thread's engine read file `path` (or found it missing, or checked whether it is
/// there) as it is now.
pub fn note(path: &Path) {
    if is_noting() {
        let stamp = stamp(path);
        noting(|noted| noted.files.push((path.to_path_buf(), stamp)));
    }
}

/// Notes that this thread's engine listed directory `path` as it is now.
pub fn note_listing(path: &Path) {
    if is_noting() {
        let names = listing(path);
        noting(|noted| noted.listings.push((path.to_path_buf(), names)));
    }
}

/// Notes that this thread's engine read something whose sameness later cannot be checked.
pub fn note_unchecked(why: &str) {
    UNCHECKED.with(|unchecked| {
        if let Ok(mut unchecked) = unchecked.try_borrow_mut() {
            unchecked.get_or_insert_with(|| why.to_string());
        }
    });
    noting(|noted| {
        noted.unchecked.get_or_insert_with(|| why.to_string());
    });
}

fn listing(path: &Path) -> Option<Vec<std::ffi::OsString>> {
    let mut names: Vec<std::ffi::OsString> = std::fs::read_dir(path).ok()?.map(|entry| entry.map(|entry| entry.file_name())).collect::<Result<_, _>>().ok()?;
    names.sort();
    Some(names)
}

impl Noted {
    /// What differs from what was read (None: every file and listing is as it was then).
    pub fn changed(&self) -> Option<String> {
        if let Some(why) = &self.unchecked {
            return Some(why.clone());
        }
        if let Some((path, _)) = self.files.iter().find(|(path, was)| stamp(path) != *was) {
            return Some(format!("{} changed", path.display()));
        }
        if let Some((path, _)) = self.listings.iter().find(|(path, was)| listing(path) != *was) {
            return Some(format!("{}'s entries changed", path.display()));
        }
        None
    }

    /// How many files and listings it holds.
    pub fn len(&self) -> usize {
        self.files.len() + self.listings.len()
    }
}

/// Writes `bytes` to `path` whole or not at all: to a temporary file of its own beside it (named by
/// the process and a count, so a service's workers writing the same cache file at once never share
/// one), then renamed over it.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let count = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut name = path.as_os_str().to_owned();
    name.push(format!(".{}-{count}.tmp", std::process::id()));
    let temporary = std::path::PathBuf::from(name);
    std::fs::write(&temporary, bytes)?;
    std::fs::rename(&temporary, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&temporary);
    })
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "bundles")]
    #[test]
    fn file_identity_distinguishes_copies_from_hard_links() {
        let root = std::env::temp_dir().join(format!("ree-file-id-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("file");
        let link = root.join("link");
        let copy = root.join("copy");
        std::fs::write(&file, "same content").unwrap();
        std::fs::hard_link(&file, &link).unwrap();
        std::fs::copy(&file, &copy).unwrap();
        assert!(super::same_file(&file, &link));
        assert!(!super::same_file(&file, &copy));
        assert!(!super::same_file(&file, &root.join("missing")));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_stamp_changes_with_the_file() {
        let file = std::env::temp_dir().join(format!("ree-stamp-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        assert!(super::stamp(&file).is_none());
        std::fs::write(&file, b"one").unwrap();
        let first = super::stamp(&file).unwrap();
        assert_eq!((first.len, first.directory), (3, false));
        assert!(super::is_file(&file));
        std::fs::write(&file, b"three").unwrap();
        assert_ne!(super::stamp(&file).unwrap(), first);
        assert!(super::stamp(&std::env::temp_dir()).unwrap().directory);
        let listed = std::fs::read_dir(file.parent().unwrap()).unwrap().flatten().find(|entry| entry.path() == file).unwrap();
        assert_eq!(super::entry_stamp(&listed), super::stamp(&file), "a listing's stamp is the file's");
        std::fs::remove_file(&file).unwrap();
    }
}
