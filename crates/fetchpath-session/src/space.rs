//! The disk reserve (FP-101, contract D6): free space the engine leaves on
//! every drive it saves to. Downloads wait for space rather than fail, and a
//! drive is never filled past the reserve by Fetchpath.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

const GIB: u64 = 1024 * 1024 * 1024;

/// The automatic reserve's floor: 5 GiB (decision O5).
pub const AUTOMATIC_RESERVE_FLOOR: u64 = 5 * GIB;

/// The "no limit" choice: a one-byte reserve, so Fetchpath leaves nothing
/// aside and a download waits only when the drive truly has no room.
pub const NO_RESERVE: u64 = 1;

/// Space a stopped download of unknown size needs above the reserve before
/// it starts again, so it does not stop and start at the boundary.
pub const RESTART_MARGIN: u64 = GIB / 2;

/// A drive's size and the space this user may still write on it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Space {
    pub free: u64,
    pub total: u64,
}

/// The reserve for a drive: the person's choice, or the larger of 5 GiB and
/// 5 % of the drive when they chose none (`chosen` is 0).
pub fn reserve(chosen: u64, total: u64) -> u64 {
    if chosen > 0 {
        chosen
    } else {
        AUTOMATIC_RESERVE_FLOOR.max(total / 20)
    }
}

/// What one pass of the queue has promised on each drive: the free space
/// seen, and the bytes still due to downloads already running or just
/// started there.
pub(crate) struct Budget {
    chosen: u64,
    roots: HashMap<PathBuf, Option<PathBuf>>,
    drives: HashMap<PathBuf, Drive>,
    probe: Box<dyn Fn(&Path) -> Measurement>,
}

struct Drive {
    space: Space,
    promised: u64,
}

impl Budget {
    pub(crate) fn with_probe(
        chosen: u64,
        probe: impl Fn(&Path) -> Option<(PathBuf, Space)> + 'static,
    ) -> Self {
        Self {
            chosen,
            roots: HashMap::new(),
            drives: HashMap::new(),
            probe: Box::new(probe),
        }
    }

    /// The drive a destination is on, measured once per pass. `None` when
    /// it cannot be measured, which never holds a download back.
    fn drive(&mut self, destination: &Path) -> Option<&mut Drive> {
        let folder = destination.parent().unwrap_or(destination).to_path_buf();
        let root = match self.roots.get(&folder) {
            Some(root) => root.clone(),
            None => {
                let measured = (self.probe)(&folder);
                let root = measured.as_ref().map(|(root, _)| root.clone());
                if let Some((root, space)) = measured {
                    self.drives
                        .entry(root)
                        .or_insert(Drive { space, promised: 0 });
                }
                self.roots.insert(folder, root.clone());
                root
            }
        }?;
        self.drives.get_mut(&root)
    }

    /// Counts bytes a running download still has to write.
    pub(crate) fn running(&mut self, destination: &Path, remaining: u64) {
        if let Some(drive) = self.drive(destination) {
            drive.promised = drive.promised.saturating_add(remaining);
        }
    }

    /// Whether a download may start: with a known remaining size, that much
    /// must fit above the reserve after what is already promised; with an
    /// unknown one, there must be room above the reserve (and the restart
    /// margin, for one stopped at the reserve before). A download that
    /// starts is counted.
    pub(crate) fn admit(
        &mut self,
        destination: &Path,
        remaining: Option<u64>,
        stopped: bool,
    ) -> bool {
        let chosen = self.chosen;
        let Some(drive) = self.drive(destination) else {
            return true;
        };
        let reserve = reserve(chosen, drive.space.total);
        let available = drive.space.free.saturating_sub(drive.promised);
        match remaining {
            Some(remaining) => {
                let fits = available >= reserve.saturating_add(remaining);
                if fits {
                    drive.promised = drive.promised.saturating_add(remaining);
                }
                fits
            }
            None => {
                let margin = if stopped { RESTART_MARGIN } else { 0 };
                available > reserve.saturating_add(margin)
            }
        }
    }

    /// Whether a drive has fallen below its reserve, so a running download
    /// of unknown size must stop at its checkpoint.
    pub(crate) fn below_reserve(&mut self, destination: &Path) -> bool {
        let chosen = self.chosen;
        self.drive(destination)
            .is_some_and(|drive| drive.space.free < reserve(chosen, drive.space.total))
    }
}

/// How long a measurement of a folder's drive is reused.
const MEASUREMENT_TTL: Duration = Duration::from_secs(2);

/// The longest a caller waits for a folder's very first measurement. A dead
/// or slow share is measured on its own thread and reads as unmeasurable
/// (never holding a download back) until it answers.
const FIRST_MEASUREMENT_WAIT: Duration = Duration::from_millis(100);

type Measurement = Option<(PathBuf, Space)>;

#[derive(Default)]
struct Entry {
    measured: Option<(Instant, Measurement)>,
    inflight: bool,
}

/// Drive measurements taken off the caller's thread, so a queue lock is
/// never held across a probe of a slow or unreachable drive.
pub(crate) struct SpaceCache {
    probe: fn(&Path) -> Measurement,
    entries: Mutex<HashMap<PathBuf, Entry>>,
    ready: Condvar,
}

impl SpaceCache {
    pub(crate) fn new(probe: fn(&Path) -> Measurement) -> Arc<Self> {
        Arc::new(Self {
            probe,
            entries: Mutex::default(),
            ready: Condvar::new(),
        })
    }

    /// The last measurement of `folder`'s drive, refreshed in the background
    /// once it is older than the TTL.
    pub(crate) fn lookup(self: &Arc<Self>, folder: &Path) -> Measurement {
        let mut entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        let entry = entries.entry(folder.to_path_buf()).or_default();
        if let Some((at, value)) = &entry.measured
            && at.elapsed() < MEASUREMENT_TTL
        {
            return value.clone();
        }
        let stale = entry.measured.as_ref().map(|(_, value)| value.clone());
        let started = !entry.inflight;
        if started {
            entry.inflight = true;
            let cache = Arc::clone(self);
            let folder = folder.to_path_buf();
            std::thread::spawn(move || {
                let value = (cache.probe)(&folder);
                let mut entries = cache.entries.lock().unwrap_or_else(|p| p.into_inner());
                let entry = entries.entry(folder).or_default();
                entry.measured = Some((Instant::now(), value));
                entry.inflight = false;
                cache.ready.notify_all();
            });
        }
        match stale {
            Some(value) => value,
            None if started => {
                let (entries, _) = self
                    .ready
                    .wait_timeout_while(entries, FIRST_MEASUREMENT_WAIT, |entries| {
                        entries.get(folder).is_none_or(|entry| entry.inflight)
                    })
                    .unwrap_or_else(|p| p.into_inner());
                entries
                    .get(folder)
                    .and_then(|entry| entry.measured.as_ref())
                    .and_then(|(_, value)| value.clone())
            }
            None => None,
        }
    }

    /// A budget for one pass of the queue that reads this cache.
    pub(crate) fn budget(self: &Arc<Self>, chosen: u64) -> Budget {
        let cache = Arc::clone(self);
        Budget::with_probe(chosen, move |folder| cache.lookup(folder))
    }
}

/// The drive holding `folder` (its nearest existing ancestor) and its space.
#[cfg(windows)]
pub(crate) fn probe(folder: &Path) -> Option<(PathBuf, Space)> {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use windows_sys::Win32::Storage::FileSystem::{GetDiskFreeSpaceExW, GetVolumePathNameW};

    let existing = folder.ancestors().find(|path| path.exists())?;
    let wide: Vec<u16> = existing
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut root = [0_u16; 1024];
    // SAFETY: `wide` is NUL-terminated and `root` is writable for its
    // stated length.
    if unsafe { GetVolumePathNameW(wide.as_ptr(), root.as_mut_ptr(), root.len() as u32) } == 0 {
        return None;
    }
    let length = root.iter().position(|unit| *unit == 0)?;
    let (mut free, mut total) = (0_u64, 0_u64);
    // SAFETY: `root` is NUL-terminated (checked above) and every out pointer
    // is a valid u64; the third, total free bytes, is not needed.
    let measured =
        unsafe { GetDiskFreeSpaceExW(root.as_ptr(), &mut free, &mut total, std::ptr::null_mut()) };
    if measured == 0 || total == 0 {
        return None;
    }
    let root = PathBuf::from(std::ffi::OsString::from_wide(&root[..length]));
    Some((root, Space { free, total }))
}

#[cfg(not(windows))]
pub(crate) fn probe(_folder: &Path) -> Option<(PathBuf, Space)> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const DRIVE_TOTAL: u64 = 100 * GIB;

    /// A 100 GiB drive with 20 GiB free, whatever the folder.
    fn twenty_free(folder: &Path) -> Option<(PathBuf, Space)> {
        let _ = folder;
        Some((
            PathBuf::from("T:\\"),
            Space {
                free: 20 * GIB,
                total: DRIVE_TOTAL,
            },
        ))
    }

    fn unmeasurable(_folder: &Path) -> Option<(PathBuf, Space)> {
        None
    }

    #[test]
    fn the_automatic_reserve_is_five_gib_or_five_percent() {
        assert_eq!(reserve(0, 10 * GIB), 5 * GIB);
        assert_eq!(reserve(0, 1000 * GIB), 50 * GIB);
        assert_eq!(reserve(GIB, 1000 * GIB), GIB);
    }

    #[test]
    fn known_sizes_are_promised_so_two_cannot_share_the_same_room() {
        // 20 GiB free, 5 GiB reserve: 15 GiB to give.
        let mut budget = Budget::with_probe(0, twenty_free);
        let file = Path::new("T:\\Downloads\\a.iso");
        assert!(budget.admit(file, Some(10 * GIB), false));
        assert!(
            !budget.admit(file, Some(10 * GIB), false),
            "only 5 GiB left"
        );
        assert!(budget.admit(file, Some(4 * GIB), false));
    }

    #[test]
    fn running_downloads_count_against_what_a_new_one_may_use() {
        let mut budget = Budget::with_probe(0, twenty_free);
        let file = Path::new("T:\\Downloads\\a.iso");
        budget.running(file, 12 * GIB);
        assert!(!budget.admit(file, Some(4 * GIB), false));
        assert!(budget.admit(file, Some(3 * GIB), false));
    }

    #[test]
    fn an_unknown_size_needs_room_and_after_a_stop_a_margin() {
        let mut budget = Budget::with_probe(0, twenty_free);
        let file = Path::new("T:\\Downloads\\stream.bin");
        assert!(budget.admit(file, None, false));
        budget.running(file, 14 * GIB + GIB / 2 + 1);
        // Above the reserve, but not by the restart margin.
        assert!(budget.admit(file, None, false));
        assert!(!budget.admit(file, None, true));
    }

    #[test]
    fn a_drive_below_its_reserve_is_reported_and_an_unmeasurable_one_never_is() {
        let mut budget = Budget::with_probe(25 * GIB, twenty_free);
        assert!(budget.below_reserve(Path::new("T:\\a.bin")));
        let mut blind = Budget::with_probe(0, unmeasurable);
        assert!(!blind.below_reserve(Path::new("T:\\a.bin")));
        assert!(blind.admit(Path::new("T:\\a.bin"), Some(u64::MAX), false));
    }

    #[cfg(windows)]
    #[test]
    fn the_real_probe_measures_the_temporary_folder() {
        let dir = std::env::temp_dir();
        let (root, space) = probe(&dir.join("not-yet").join("file.bin")).expect("measured");
        assert!(dir.starts_with(&root) || root.as_os_str().len() <= 4);
        assert!(space.total > 0 && space.free <= space.total);
    }
}
