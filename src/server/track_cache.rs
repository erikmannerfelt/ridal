//! Every radargram's track, read once per file rather than once per
//! request.
//!
//! A track is a full read of a file's `easting`, `northing` and `time` and
//! a simplification of them, a few milliseconds a file under the NetCDF
//! library lock. Discovery needs each one for the length on its card, and
//! every group map and viewer page asked for them again, so a catalog of
//! 700 radargrams read 700 files to draw its maps and 700 more at every
//! rediscovery. Here a track is read once per file and kept, in memory
//! and on disk, so a restart does not read them all again either.
//!
//! Disk layout: `<cache_dir>/tracks/<key>.json`, one file per radargram
//! file. The key hashes [`TRACK_VERSION`], the file's canonical path and
//! its [`FileStamp`], so a rewritten file, or a new way of building
//! tracks, is a new name and nothing is invalidated explicitly.
//! [`TrackCache::retain`] removes what discovery no longer finds. Like
//! the rest of `cache/`, deleting any of it is always safe.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use super::overview_cache::{write_atomically, FileStamp};
use super::track::{read_track_from_netcdf, Track, TRACK_VERSION};

/// Subdirectory of the project's cache directory.
const TRACK_DIR: &str = "tracks";

/// What a file's track read gave. A failure is kept too, so a file with no
/// readable track is not reopened on every request for its group.
type Read = Result<Arc<Track>, String>;

/// The tracks of one catalog's files.
#[derive(Debug, Default)]
pub struct TrackCache {
    /// `None` keeps tracks in memory only: a bare directory has nowhere of
    /// its own to write, and tests need no disk.
    dir: Option<PathBuf>,
    /// By canonical path, with the stamp the read was made against.
    memory: Mutex<HashMap<PathBuf, (FileStamp, Read)>>,
    /// An unwritable cache directory is reported once, not once per file.
    warned: AtomicBool,
}

impl TrackCache {
    /// A cache that keeps tracks only as long as the process.
    pub fn in_memory() -> Self {
        Self::default()
    }

    /// A cache that also keeps tracks under `cache_dir`. Nothing is created
    /// until the first write.
    pub fn on_disk(cache_dir: &Path) -> Self {
        Self {
            dir: Some(cache_dir.join(TRACK_DIR)),
            ..Self::default()
        }
    }

    /// The track of the file at `path`, read only if this cache has none
    /// for the file as it is now.
    ///
    /// A file that cannot be stat'ed is read every time rather than keyed
    /// on a guess, as the overview cache does.
    pub fn get(&self, path: &Path) -> Read {
        let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let Some(stamp) = FileStamp::of(&path) else {
            return read_track_from_netcdf(&path).map(Arc::new);
        };
        if let Some((cached_stamp, read)) = self.lock().get(&path) {
            if *cached_stamp == stamp {
                return read.clone();
            }
        }
        let read = match self.read_disk(&path, stamp) {
            Some(track) => Ok(Arc::new(track)),
            None => read_track_from_netcdf(&path)
                .map(|track| Arc::new(self.write_disk(&path, stamp, track))),
        };
        self.lock().insert(path, (stamp, read.clone()));
        read
    }

    /// Forget every file not in `live`, in memory and on disk, returning
    /// how many disk entries were removed.
    ///
    /// Run after discovery, which has just read every live file through
    /// this cache, so what is left in memory names exactly the disk
    /// entries worth keeping. Without it a project that reprocesses its
    /// radargrams keeps one track per version of each file. Only `.json`
    /// files are removed; anything else in the directory is left alone.
    pub fn retain(&self, live: &[PathBuf]) -> usize {
        let live: HashSet<PathBuf> = live
            .iter()
            .map(|p| p.canonicalize().unwrap_or_else(|_| p.clone()))
            .collect();
        let keep: HashSet<String> = {
            let mut memory = self.lock();
            memory.retain(|path, _| live.contains(path));
            memory
                .iter()
                .map(|(path, (stamp, _))| key(path, *stamp))
                .collect()
        };
        let Some(dir) = &self.dir else {
            return 0;
        };
        let Ok(entries) = std::fs::read_dir(dir) else {
            return 0;
        };
        let mut removed = 0;
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let is_track = path.extension().is_some_and(|e| e == "json");
            if is_track && !keep.contains(stem) && std::fs::remove_file(&path).is_ok() {
                removed += 1;
            }
        }
        removed
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<PathBuf, (FileStamp, Read)>> {
        // A panic while holding this lock leaves a map of finished reads,
        // each of which is still right.
        self.memory.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn disk_path(&self, path: &Path, stamp: FileStamp) -> Option<PathBuf> {
        let dir = self.dir.as_ref()?;
        Some(dir.join(format!("{}.json", key(path, stamp))))
    }

    /// The stored track, or `None` on a miss. A file that exists but does
    /// not parse is a miss too, and the write that follows replaces it.
    fn read_disk(&self, path: &Path, stamp: FileStamp) -> Option<Track> {
        let bytes = std::fs::read(self.disk_path(path, stamp)?).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// Store `track` and return what was stored, read back from disk, so a
    /// track served from a cold cache is the one a warm cache would serve.
    ///
    /// A failed write is not a failed read: `track` is returned as read,
    /// and the failure is reported once.
    fn write_disk(&self, path: &Path, stamp: FileStamp, track: Track) -> Track {
        let Some(disk_path) = self.disk_path(path, stamp) else {
            return track;
        };
        let stored = serde_json::to_vec(&track)
            .map_err(std::io::Error::other)
            .and_then(|bytes| write_atomically(&disk_path, &bytes))
            .and_then(|()| std::fs::read(&disk_path))
            .and_then(|bytes| serde_json::from_slice(&bytes).map_err(std::io::Error::other));
        match stored {
            Ok(stored) => stored,
            Err(e) => {
                if !self.warned.swap(true, Ordering::Relaxed) {
                    eprintln!(
                        "Warning: could not store a track in {}: {e}. \
                         Tracks will be read again after every restart.",
                        disk_path.parent().unwrap_or(&disk_path).display()
                    );
                }
                track
            }
        }
    }
}

/// The disk name of a file's track: everything that says which track it
/// is, hashed.
fn key(path: &Path, stamp: FileStamp) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"ridal-track-file-v1");
    hasher.update(&TRACK_VERSION.to_le_bytes());
    hasher.update(path.as_os_str().as_encoded_bytes());
    hasher.update(&stamp.to_bytes());
    hasher.finalize().to_hex()[..32].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A small processed radargram with a real track, as `track.rs`'s own
    /// file tests make one.
    fn processed(dir: &Path, name: &str) -> PathBuf {
        let nc_path = dir.join(format!("{name}.nc"));
        let params = crate::gpr::RunParams {
            filepaths: vec![PathBuf::from(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/mala/dronbreen-20220329-DAT_0237_A1.rad"
            ))],
            output_path: Some(nc_path.clone()),
            track_path: None,
            render_path: None,
            radargram_id: Some(name.to_string()),
            display_name: None,
            options: crate::gpr::ProcessingOptions {
                quiet: true,
                steps: vec!["subset(0 300 0 50)".to_string()],
                ..Default::default()
            },
        };
        crate::gpr::run(params).unwrap();
        nc_path
    }

    fn stored(dir: &Path) -> Vec<PathBuf> {
        let mut files: Vec<PathBuf> = std::fs::read_dir(dir.join(TRACK_DIR))
            .map(|entries| entries.flatten().map(|e| e.path()).collect())
            .unwrap_or_default();
        files.sort();
        files
    }

    /// Move every stored vertex to (0, 0), so a track that comes back that
    /// way can only have come from the disk.
    fn tamper(path: &Path) {
        let mut track: Track = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        for vertex in track
            .segments
            .iter_mut()
            .flat_map(|s| s.vertices.iter_mut())
        {
            vertex.lon = 0.0;
            vertex.lat = 0.0;
        }
        std::fs::write(path, serde_json::to_vec(&track).unwrap()).unwrap();
    }

    fn is_tampered(track: &Track) -> bool {
        track.segments[0].vertices[0].lon == 0.0
    }

    #[test]
    #[serial_test::serial(netcdf)]
    fn a_track_is_read_once_and_kept_in_memory_and_on_disk() {
        let data = tempfile::tempdir().unwrap();
        let cache_dir = tempfile::tempdir().unwrap();
        let nc = processed(data.path(), "cached-track");
        let cache = TrackCache::on_disk(cache_dir.path());

        let track = cache.get(&nc).unwrap();
        assert_eq!(*track, read_track_from_netcdf(&nc).unwrap());
        let files = stored(cache_dir.path());
        assert_eq!(files.len(), 1, "{files:?}");
        let name = files[0].file_name().unwrap().to_string_lossy().to_string();
        assert!(name.ends_with(".json") && !name.contains("tmp"), "{name}");

        tamper(&files[0]);
        // This cache answers from memory, without looking at the disk ...
        assert!(!is_tampered(&cache.get(&nc).unwrap()));
        // ... and a new one, as after a restart, from the disk, without
        // opening the radargram.
        let restarted = TrackCache::on_disk(cache_dir.path());
        assert!(is_tampered(&restarted.get(&nc).unwrap()));
    }

    #[test]
    #[serial_test::serial(netcdf)]
    fn a_rewritten_file_is_read_again_and_its_old_track_removed() {
        let data = tempfile::tempdir().unwrap();
        let cache_dir = tempfile::tempdir().unwrap();
        let nc = processed(data.path(), "rewritten-track");
        let cache = TrackCache::on_disk(cache_dir.path());
        cache.get(&nc).unwrap();
        tamper(&stored(cache_dir.path())[0]);
        let restarted = TrackCache::on_disk(cache_dir.path());
        assert!(is_tampered(&restarted.get(&nc).unwrap()));

        // The same file, written again: a new stamp, so neither the memory
        // nor the disk entry is the file's track any longer.
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(60);
        std::fs::File::options()
            .write(true)
            .open(&nc)
            .unwrap()
            .set_modified(later)
            .unwrap();
        assert!(!is_tampered(&restarted.get(&nc).unwrap()));
        assert_eq!(stored(cache_dir.path()).len(), 2);

        // Discovery's sweep keeps the current one only, and forgets a file
        // that is gone altogether.
        assert_eq!(restarted.retain(std::slice::from_ref(&nc)), 1);
        assert_eq!(stored(cache_dir.path()).len(), 1);
        assert!(!is_tampered(
            &TrackCache::on_disk(cache_dir.path()).get(&nc).unwrap()
        ));
        assert_eq!(restarted.retain(&[]), 1);
        assert!(stored(cache_dir.path()).is_empty());
    }

    #[test]
    fn an_unreadable_file_is_an_error_and_writes_nothing() {
        let data = tempfile::tempdir().unwrap();
        let cache_dir = tempfile::tempdir().unwrap();
        let bogus = data.path().join("not-a-radargram.nc");
        std::fs::write(&bogus, b"not NetCDF").unwrap();
        let cache = TrackCache::on_disk(cache_dir.path());
        assert!(cache.get(&bogus).is_err());
        assert!(cache.get(&bogus).is_err());
        assert!(stored(cache_dir.path()).is_empty());
    }

    #[test]
    #[serial_test::serial(netcdf)]
    fn an_unwritable_cache_still_returns_the_track() {
        let data = tempfile::tempdir().unwrap();
        let nc = processed(data.path(), "unwritable-cache");
        // The cache directory is a file, so nothing can be created under it.
        let blocker = data.path().join("cache");
        std::fs::write(&blocker, b"not a directory").unwrap();
        let cache = TrackCache::on_disk(&blocker);
        assert_eq!(
            *cache.get(&nc).unwrap(),
            read_track_from_netcdf(&nc).unwrap()
        );
    }
}
