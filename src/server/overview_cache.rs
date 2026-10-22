//! Encoded overviews kept on disk, in the project's cache directory (#180).
//!
//! An overview's input is the whole radargram, so building one is a full
//! read of the file, and the in-memory [`super::render_service::RenderCache`]
//! loses every one of them at a restart. Hundreds of index thumbnails
//! requested at once after a restart were each a full-file read; from here
//! they are a small file read each, and a hit opens no NetCDF at all.
//!
//! Layout: `<cache_dir>/overviews/<revision_id>/<profile>-<key>.<png|jpg>`.
//! One directory per revision so that [`OverviewDiskCache::harvest`] can
//! drop everything for a revision the catalog no longer has with one
//! `remove_dir_all`, and the profile's [`profile_tag`] at the front of each
//! name so that it can drop the overviews of a profile that was edited or
//! deleted (#183). Like the rest of `cache/`, deleting any of it is always
//! safe.
//!
//! Only overviews go here. Chunks are cheap (one chunk's source window), and
//! there are far too many of them to be worth keeping.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::identity::RevisionId;
use crate::render::profile::{ImageFormat, RenderProfile};

use super::render_service::RenderVariantId;

/// Subdirectory of the project's cache directory.
const OVERVIEW_DIR: &str = "overviews";

/// Length of a [`profile_tag`], in hex characters. Long enough that two
/// profiles of one project do not share a tag; it names a profile, and the
/// rest of the file name still hashes everything.
const PROFILE_TAG_LEN: usize = 12;

/// What an overview's file name starts with: a hash of everything about
/// `profile` that affects its pixels, so it changes when the profile is
/// edited, and a profile nobody can ask for any more can be recognised by
/// it.
pub fn profile_tag(profile: &RenderProfile) -> String {
    let hash = blake3::hash(profile.cache_key_fragment().as_bytes());
    hash.to_hex()[..PROFILE_TAG_LEN].to_string()
}

/// The overview store for one project. Cheap to clone; clones share the
/// "already warned" flag, so an unwritable cache directory is reported
/// once per server rather than once per thumbnail.
#[derive(Debug, Clone)]
pub struct OverviewDiskCache {
    dir: PathBuf,
    warned: Arc<AtomicBool>,
}

/// What, beyond the render variant, says which file an overview was made
/// from: its length and modification time.
///
/// The revision id inside the variant is a *declared* identity -- a
/// radargram id and processing time the file states about itself -- and
/// deliberately ignores the filesystem (#117), so that moving or copying a
/// file does not make it a new revision. That is right for provenance and
/// wrong for a cache that outlives the process: a file rewritten in place
/// with the same declared processing time would be served its old
/// overview indefinitely. Folding in the stamp costs a rebuild after a
/// copy, which is cheap, and rules out a stale image, which is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileStamp {
    len: u64,
    modified_nanos: u128,
}

impl FileStamp {
    /// `None` when the file cannot be stat'ed or has no modification time
    /// on this platform; the caller then skips the disk cache rather than
    /// keying on a guess.
    pub fn of(path: &Path) -> Option<Self> {
        let metadata = std::fs::metadata(path).ok()?;
        let modified = metadata.modified().ok()?;
        let modified_nanos = modified
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_nanos();
        Some(Self {
            len: metadata.len(),
            modified_nanos,
        })
    }

    /// The stamp as bytes, for a cache key.
    pub fn to_bytes(self) -> [u8; 24] {
        let mut bytes = [0; 24];
        bytes[..8].copy_from_slice(&self.len.to_le_bytes());
        bytes[8..].copy_from_slice(&self.modified_nanos.to_le_bytes());
        bytes
    }
}

impl OverviewDiskCache {
    /// The store under a project's `cache_dir`. Nothing is created until
    /// the first write.
    pub fn new(cache_dir: &Path) -> Self {
        Self {
            dir: cache_dir.join(OVERVIEW_DIR),
            warned: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Where one overview lives.
    ///
    /// The file name hashes everything that changes the pixels -- the
    /// variant (revision, view, profile, elevation range, renderer
    /// versions), the overview size and the file stamp -- so nothing needs
    /// invalidating explicitly: a change is a different name, and the old
    /// file is unreachable until [`Self::harvest`] or a cache wipe removes
    /// it.
    fn path_of(
        &self,
        revision: &RevisionId,
        variant: &RenderVariantId,
        stamp: FileStamp,
        width: usize,
        height: usize,
        profile: &RenderProfile,
    ) -> PathBuf {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"ridal-overview-file-v1");
        hasher.update(variant.as_str().as_bytes());
        hasher.update(&stamp.to_bytes());
        hasher.update(&(width as u64).to_le_bytes());
        hasher.update(&(height as u64).to_le_bytes());
        let name = &hasher.finalize().to_hex()[..32];
        let extension = match profile.format {
            ImageFormat::Png => "png",
            ImageFormat::Jpeg { .. } => "jpg",
        };
        self.dir
            .join(revision.as_str())
            .join(format!("{}-{name}.{extension}", profile_tag(profile)))
    }

    /// The cached bytes, or `None` on a miss. A file that exists but cannot
    /// be read is a miss too: the overview is rebuilt and the write
    /// replaces it.
    #[allow(clippy::too_many_arguments)]
    pub fn get(
        &self,
        revision: &RevisionId,
        variant: &RenderVariantId,
        stamp: FileStamp,
        width: usize,
        height: usize,
        profile: &RenderProfile,
    ) -> Option<Vec<u8>> {
        std::fs::read(self.path_of(revision, variant, stamp, width, height, profile)).ok()
    }

    /// Store `bytes` and return what was stored, re-read from disk.
    ///
    /// Written to a uniquely named sibling and renamed into place, so a
    /// reader -- another request, or another server sharing the cache
    /// directory -- sees either no file or a complete one, never a
    /// truncated image that would be served as if it were whole.
    ///
    /// A failed write is not a failed render: the caller still has the
    /// image. It is reported once and `bytes` is returned as rendered.
    #[allow(clippy::too_many_arguments)]
    pub fn put(
        &self,
        revision: &RevisionId,
        variant: &RenderVariantId,
        stamp: FileStamp,
        width: usize,
        height: usize,
        profile: &RenderProfile,
        bytes: Vec<u8>,
    ) -> Vec<u8> {
        let path = self.path_of(revision, variant, stamp, width, height, profile);
        match write_atomically(&path, &bytes).and_then(|()| std::fs::read(&path)) {
            Ok(stored) => stored,
            Err(e) => {
                if !self.warned.swap(true, Ordering::Relaxed) {
                    eprintln!(
                        "Warning: could not store an overview in {}: {e}. \
                         Overviews will be rebuilt after every restart.",
                        self.dir.display()
                    );
                }
                bytes
            }
        }
    }

    /// Remove every revision directory not in `revisions`, and in the
    /// directories that stay, every overview whose [`profile_tag`] is not in
    /// `profiles`; returning how many directories and files were removed.
    ///
    /// Run when the catalog is (re)discovered. Without it a project that
    /// reprocesses its radargrams accumulates one set of overviews per
    /// processing run, and one that edits its render profiles one set per
    /// edit (#183). A file named before profile tags existed has none, and
    /// goes too. A stray file beside the revision directories, or an entry
    /// whose name is not UTF-8, is left alone.
    pub fn harvest(&self, revisions: &HashSet<String>, profiles: &HashSet<String>) -> usize {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return 0;
        };
        let mut removed = 0;
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if !kind.is_dir() {
                continue;
            }
            if !revisions.contains(name) {
                if std::fs::remove_dir_all(entry.path()).is_ok() {
                    removed += 1;
                }
                continue;
            }
            let Ok(files) = std::fs::read_dir(entry.path()) else {
                continue;
            };
            for file in files.flatten() {
                let file_name = file.file_name();
                let Some(file_name) = file_name.to_str() else {
                    continue;
                };
                let live = file_name
                    .split_once('-')
                    .is_some_and(|(tag, _)| profiles.contains(tag));
                if !live && std::fs::remove_file(file.path()).is_ok() {
                    removed += 1;
                }
            }
        }
        removed
    }
}

/// Write `bytes` to `path` through a temporary sibling and a rename.
///
/// The temporary name keeps the image extension last
/// (`<key>.tmp-<pid>-<n>.png`) and is unique per process and call, so two
/// requests or two servers writing the same overview never share one.
pub(super) fn write_atomically(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("cache path has no parent"))?;
    std::fs::create_dir_all(parent)?;
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("overview");
    let extension = path.extension().and_then(|s| s.to_str()).unwrap_or("bin");
    let temporary = parent.join(format!(
        "{stem}.tmp-{}-{}.{extension}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let result = std::fs::write(&temporary, bytes).and_then(|()| std::fs::rename(&temporary, path));
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::RadargramId;
    use crate::render::profile::{DatasetView, RenderProfile};
    use crate::render::topo::ElevationRange;

    fn revision(name: &str) -> RevisionId {
        RevisionId::fingerprint_v1(&RadargramId::new(name).unwrap(), "2026-10-01T00:00:00Z")
    }

    fn variant(revision: &RevisionId) -> RenderVariantId {
        RenderVariantId::compute(
            revision,
            DatasetView::Standard,
            &RenderProfile::default_profile(),
            ElevationRange::NONE,
        )
    }

    const STAMP: FileStamp = FileStamp {
        len: 10,
        modified_nanos: 20,
    };

    /// The default profile, as PNG: what the tests store.
    fn png() -> RenderProfile {
        RenderProfile {
            format: ImageFormat::Png,
            ..RenderProfile::default_profile()
        }
    }

    fn tags(profiles: &[&RenderProfile]) -> HashSet<String> {
        profiles.iter().map(|p| profile_tag(p)).collect()
    }

    #[test]
    fn a_stored_overview_is_read_back_and_nothing_else_is() {
        let dir = tempfile::tempdir().unwrap();
        let cache = OverviewDiskCache::new(dir.path());
        let rev = revision("line-a");
        let var = variant(&rev);
        let png = png();

        assert_eq!(cache.get(&rev, &var, STAMP, 512, 100, &png), None);
        let stored = cache.put(&rev, &var, STAMP, 512, 100, &png, vec![1, 2, 3]);
        assert_eq!(stored, vec![1, 2, 3]);
        assert_eq!(
            cache.get(&rev, &var, STAMP, 512, 100, &png),
            Some(vec![1, 2, 3])
        );

        // Each input that changes the pixels or says which file they came
        // from is part of the key.
        assert_eq!(cache.get(&rev, &var, STAMP, 256, 50, &png), None);
        let touched = FileStamp {
            modified_nanos: 21,
            ..STAMP
        };
        assert_eq!(cache.get(&rev, &var, touched, 512, 100, &png), None);
        let jpeg = RenderProfile::default_profile();
        assert_eq!(cache.get(&rev, &var, STAMP, 512, 100, &jpeg), None);
        let other = RenderVariantId::compute(
            &rev,
            DatasetView::Standard,
            &RenderProfile::by_name("seismic").unwrap(),
            ElevationRange::NONE,
        );
        assert_eq!(cache.get(&rev, &other, STAMP, 512, 100, &png), None);
    }

    #[test]
    fn a_write_leaves_no_temporary_files_behind() {
        let dir = tempfile::tempdir().unwrap();
        let cache = OverviewDiskCache::new(dir.path());
        let rev = revision("line-a");
        cache.put(&rev, &variant(&rev), STAMP, 8, 8, &png(), vec![0; 16]);
        let files: Vec<_> = std::fs::read_dir(dir.path().join(OVERVIEW_DIR).join(rev.as_str()))
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(files.len(), 1, "{files:?}");
        assert!(files[0].ends_with(".png") && !files[0].contains("tmp"));
        assert!(files[0].starts_with(&format!("{}-", profile_tag(&png()))));
    }

    #[test]
    fn an_unwritable_cache_still_returns_the_render() {
        // The cache directory is a file, so nothing can be created under
        // it: the image the caller rendered must still come back.
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("cache");
        std::fs::write(&blocker, b"not a directory").unwrap();
        let cache = OverviewDiskCache::new(&blocker);
        let rev = revision("line-a");
        let bytes = cache.put(&rev, &variant(&rev), STAMP, 8, 8, &png(), vec![7]);
        assert_eq!(bytes, vec![7]);
    }

    #[test]
    fn harvest_removes_only_revisions_the_catalog_no_longer_has() {
        let dir = tempfile::tempdir().unwrap();
        let cache = OverviewDiskCache::new(dir.path());
        let (live, stale) = (revision("line-a"), revision("line-b"));
        for rev in [&live, &stale] {
            cache.put(rev, &variant(rev), STAMP, 8, 8, &png(), vec![1]);
        }
        let stray = dir.path().join(OVERVIEW_DIR).join("README");
        std::fs::write(&stray, b"left alone").unwrap();

        let keep: HashSet<String> = [live.to_string()].into();
        assert_eq!(cache.harvest(&keep, &tags(&[&png()])), 1);
        assert!(cache
            .get(&live, &variant(&live), STAMP, 8, 8, &png())
            .is_some());
        assert!(cache
            .get(&stale, &variant(&stale), STAMP, 8, 8, &png())
            .is_none());
        assert!(stray.exists());
    }

    #[test]
    fn harvest_removes_the_overviews_of_a_profile_nobody_can_ask_for() {
        // An edited or deleted project profile (#183) leaves its overviews
        // behind under a tag no offered profile has.
        let dir = tempfile::tempdir().unwrap();
        let cache = OverviewDiskCache::new(dir.path());
        let rev = revision("line-a");
        let kept = png();
        let edited = RenderProfile {
            contrast: 1.5,
            ..png()
        };
        for profile in [&kept, &edited] {
            cache.put(&rev, &variant(&rev), STAMP, 8, 8, profile, vec![1]);
        }
        // A file from before tags existed has none to match.
        let untagged = dir
            .path()
            .join(OVERVIEW_DIR)
            .join(rev.as_str())
            .join("0123456789abcdef0123456789abcdef.png");
        std::fs::write(&untagged, b"old").unwrap();

        let revisions: HashSet<String> = [rev.to_string()].into();
        assert_eq!(cache.harvest(&revisions, &tags(&[&kept])), 2);
        assert!(cache
            .get(&rev, &variant(&rev), STAMP, 8, 8, &kept)
            .is_some());
        assert!(cache
            .get(&rev, &variant(&rev), STAMP, 8, 8, &edited)
            .is_none());
        assert!(!untagged.exists());
    }
}
