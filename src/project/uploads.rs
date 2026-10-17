//! Uploads sent in chunks, which survive a dropped connection (#396).
//!
//! A file too large to send in one request -- a DEM of a glacier system, a
//! long recording, a processed radargram over a proxy's request limit -- is
//! declared first, then sent a chunk at a time, and resumed from wherever
//! the server got to when a chunk fails. Each upload is two files in
//! `ridal_data/uploads/`:
//!
//! - `<id>.json`, what was declared: the file's name, its size and who is
//!   sending it ([`Upload`]).
//! - `<id>.part`, what has arrived so far.
//!
//! The offset is the `.part` file's length and nothing else. A counter kept
//! beside it could disagree with the file after a crash or a chunk that
//! broke off halfway; the length cannot, and the next chunk is asked to
//! start exactly there.
//!
//! A finished upload is not a stored file of its own: whatever it was for
//! takes the `.part` away ([`move_part`], then [`remove`]) -- into the raw
//! store, or as a new radargram or a replacement -- and the upload is gone. Both files sit
//! under the project, so they count toward `max_bytes` as they grow, and
//! taking the `.part` into the raw store is a rename on one filesystem.
//!
//! An upload nobody has sent to for [`MAX_IDLE`] is deleted by [`sweep`].

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use super::store::{DocumentStore, Expectation};

/// Relative to the project's data directory.
pub const UPLOADS_DIR: &str = "uploads";

/// How long an upload may go without a chunk before [`sweep`] deletes it.
///
/// A day rather than the hour an abandoned single-request upload gets
/// ([`super::raw::SWEEP_MIN_AGE`]): resuming is the point, and the case
/// it exists for is a laptop that went to sleep overnight.
pub const MAX_IDLE: Duration = Duration::from_secs(24 * 60 * 60);

/// What was declared when an upload was started.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Upload {
    /// The file's name, which says what it is to a recording
    /// (`DAT_0001_A1.rd3`).
    pub name: String,
    /// The whole file's size in bytes.
    pub size: u64,
    /// The account sending it; `None` in `ridal gui`, which has no
    /// accounts. Nobody else may send to it, read it or take it.
    pub owner: Option<String>,
    /// When it was started, RFC 3339.
    pub created: String,
}

/// An upload id as 32 lowercase hex characters, or `None`. Paths are built
/// from it, so this is what keeps them inside the uploads directory.
pub fn valid_id(text: &str) -> Option<&str> {
    (text.len() == 32
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
    .then_some(text)
}

/// A fresh id: 128 random bits. Random rather than from the clock, since
/// an id is all it takes to find an upload once its owner is known.
fn new_id() -> Result<String, String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|e| format!("no system randomness: {e}"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

fn document(id: &str) -> PathBuf {
    Path::new(UPLOADS_DIR).join(format!("{id}.json"))
}

/// Where an existing upload's bytes are, or `None` when there is no such
/// upload.
///
/// Found by listing the uploads directory and comparing names, so that the
/// path comes from the filesystem and the id, which reaches here from a
/// request, is only ever compared. Building the path from the id was safe,
/// since [`valid_id`] allows only hex digits, but by a check CodeQL cannot
/// follow; a comparison needs no such argument. There are only ever a few
/// uploads to list.
pub fn part_path(data_dir: &Path, id: &str) -> Option<PathBuf> {
    let wanted = format!("{}.part", valid_id(id)?);
    std::fs::read_dir(data_dir.join(UPLOADS_DIR))
        .ok()?
        .flatten()
        .find(|entry| entry.file_name() == wanted.as_str())
        .map(|entry| entry.path())
}

/// Start an upload, with nothing sent yet, and answer its id.
pub fn create(store: &DocumentStore, data_dir: &Path, upload: &Upload) -> Result<String, String> {
    let id = new_id()?;
    let dir = data_dir.join(UPLOADS_DIR);
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    // Built from the id, since it is this server's own and has no file yet.
    let part = dir.join(format!("{id}.part"));
    // The `.part` first: a crash between the two leaves a file with no
    // document, which `sweep` removes, rather than a document whose bytes
    // cannot be found.
    std::fs::File::create_new(&part)
        .map_err(|e| format!("could not create {}: {e}", part.display()))?;
    let text = serde_json::to_string_pretty(upload).expect("an upload is plain data");
    if let Err(e) = store.write(&document(&id), &text, &Expectation::Absent) {
        let _ = std::fs::remove_file(&part);
        return Err(e.to_string());
    }
    Ok(id)
}

/// What was declared for an upload, or `None` if there is no such upload.
pub fn read(store: &DocumentStore, data_dir: &Path, id: &str) -> Result<Option<Upload>, String> {
    if valid_id(id).is_none() {
        return Ok(None);
    }
    let Some(text) = store.read(&document(id)).map_err(|e| e.to_string())? else {
        return Ok(None);
    };
    // Without its bytes it is not an upload that can go on.
    if part_path(data_dir, id).is_none() {
        return Ok(None);
    }
    serde_json::from_str(&text.text)
        .map(Some)
        .map_err(|e| format!("the upload '{id}' cannot be read: {e}"))
}

/// How much of an upload has arrived: its `.part` file's length.
pub fn offset(data_dir: &Path, id: &str) -> std::io::Result<u64> {
    let part = part_path(data_dir, id).ok_or(std::io::ErrorKind::NotFound)?;
    Ok(part.metadata()?.len())
}

/// When [`sweep`] will delete an upload if nothing more is sent to it.
pub fn expires_at(data_dir: &Path, id: &str) -> std::io::Result<SystemTime> {
    let part = part_path(data_dir, id).ok_or(std::io::ErrorKind::NotFound)?;
    Ok(part.metadata()?.modified()? + MAX_IDLE)
}

/// Forget an upload and delete what has arrived. `false` when there was no
/// such upload.
pub fn remove(store: &DocumentStore, data_dir: &Path, id: &str) -> Result<bool, String> {
    if valid_id(id).is_none() {
        return Ok(false);
    }
    // No `.part` after `move_part`, and only the document is left.
    let had_part = match part_path(data_dir, id).map(|part| (std::fs::remove_file(&part), part)) {
        None => false,
        Some((Ok(()), _)) => true,
        Some((Err(e), _)) if e.kind() == std::io::ErrorKind::NotFound => false,
        Some((Err(e), part)) => return Err(format!("could not delete {}: {e}", part.display())),
    };
    let had_document = store
        .remove(&document(id), &Expectation::Any)
        .map_err(|e| e.to_string())?;
    Ok(had_part || had_document)
}

/// Move a finished upload's bytes to `destination`, leaving its document
/// for [`remove`] to forget once the bytes are safely there.
///
/// A rename where it can be, which it is anywhere under the project's data
/// directory; a copy where `destination` is on another filesystem, such as
/// a radargram directory that is a mount of its own. A copy of a large
/// file takes a while, so a server does this off the async runtime.
pub fn move_part(data_dir: &Path, id: &str, destination: &Path) -> Result<(), String> {
    let part = part_path(data_dir, id).ok_or_else(|| format!("'{id}' is not an upload id"))?;
    if std::fs::rename(&part, destination).is_err() {
        std::fs::copy(&part, destination).map_err(|e| {
            format!(
                "could not move the upload to {}: {e}",
                destination.display()
            )
        })?;
        let _ = std::fs::remove_file(&part);
    }
    Ok(())
}

/// Delete every upload nobody has sent to for `max_idle`, and the halves of
/// any that a crash left without the other. Answers how many uploads went.
///
/// An upload's age is that of the newest of its files, so one that is
/// being sent to stays however long ago it was started.
pub fn sweep(data_dir: &Path, max_idle: Duration) -> Result<usize, String> {
    let dir = data_dir.join(UPLOADS_DIR);
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(format!("could not read {}: {e}", dir.display())),
    };
    // Every file by the upload it belongs to: the name up to the first
    // dot, which also catches the store's own temporaries.
    let mut uploads: std::collections::BTreeMap<String, (Vec<PathBuf>, Option<SystemTime>)> =
        std::collections::BTreeMap::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let stem = name.split('.').next().unwrap_or_default().to_string();
        let modified = entry.metadata().and_then(|m| m.modified()).ok();
        let (files, newest) = uploads.entry(stem).or_default();
        files.push(entry.path());
        *newest = (*newest).max(modified);
    }
    let mut swept = 0;
    for (files, newest) in uploads.into_values() {
        let idle = newest
            .and_then(|at| at.elapsed().ok())
            .is_some_and(|age| age >= max_idle);
        if idle {
            for file in &files {
                let _ = std::fs::remove_file(file);
            }
            swept += 1;
        }
    }
    Ok(swept)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn upload(size: u64) -> Upload {
        Upload {
            name: "DAT_0001_A1.rd3".into(),
            size,
            owner: Some("erik".into()),
            created: "2026-10-17T00:00:00Z".into(),
        }
    }

    #[test]
    fn an_upload_starts_empty_and_reads_back() {
        let dir = tempfile::tempdir().unwrap();
        let store = DocumentStore::new(dir.path().to_path_buf());
        let id = create(&store, dir.path(), &upload(10)).unwrap();
        assert!(valid_id(&id).is_some());
        assert_eq!(read(&store, dir.path(), &id).unwrap(), Some(upload(10)));
        assert_eq!(offset(dir.path(), &id).unwrap(), 0);
        assert!(expires_at(dir.path(), &id).unwrap() > SystemTime::now());
    }

    #[test]
    fn ids_that_are_not_ids_find_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let store = DocumentStore::new(dir.path().to_path_buf());
        for id in ["", "../../ridal", "ABCDEF0123456789ABCDEF0123456789"] {
            assert_eq!(read(&store, dir.path(), id).unwrap(), None);
            assert!(part_path(dir.path(), id).is_none());
            assert!(!remove(&store, dir.path(), id).unwrap());
        }
    }

    #[test]
    fn a_document_without_its_bytes_is_no_upload() {
        let dir = tempfile::tempdir().unwrap();
        let store = DocumentStore::new(dir.path().to_path_buf());
        let id = create(&store, dir.path(), &upload(10)).unwrap();
        std::fs::remove_file(part_path(dir.path(), &id).unwrap()).unwrap();
        assert_eq!(read(&store, dir.path(), &id).unwrap(), None);
    }

    #[test]
    fn taking_moves_the_bytes_and_forgets_the_upload() {
        let dir = tempfile::tempdir().unwrap();
        let store = DocumentStore::new(dir.path().to_path_buf());
        let id = create(&store, dir.path(), &upload(5)).unwrap();
        std::fs::write(part_path(dir.path(), &id).unwrap(), b"hello").unwrap();
        let destination = dir.path().join("taken");
        move_part(dir.path(), &id, &destination).unwrap();
        assert!(remove(&store, dir.path(), &id).unwrap());
        assert_eq!(std::fs::read(&destination).unwrap(), b"hello");
        assert_eq!(read(&store, dir.path(), &id).unwrap(), None);
        assert_eq!(
            std::fs::read_dir(dir.path().join(UPLOADS_DIR))
                .unwrap()
                .count(),
            0
        );
    }

    #[test]
    fn removing_deletes_both_halves() {
        let dir = tempfile::tempdir().unwrap();
        let store = DocumentStore::new(dir.path().to_path_buf());
        let id = create(&store, dir.path(), &upload(5)).unwrap();
        assert!(remove(&store, dir.path(), &id).unwrap());
        assert!(!remove(&store, dir.path(), &id).unwrap());
        assert_eq!(
            std::fs::read_dir(dir.path().join(UPLOADS_DIR))
                .unwrap()
                .count(),
            0
        );
    }

    #[test]
    fn the_sweep_takes_idle_uploads_and_orphans_and_leaves_fresh_ones() {
        let dir = tempfile::tempdir().unwrap();
        let store = DocumentStore::new(dir.path().to_path_buf());
        let idle = create(&store, dir.path(), &upload(5)).unwrap();
        let fresh = create(&store, dir.path(), &upload(5)).unwrap();
        let orphan = dir
            .path()
            .join(UPLOADS_DIR)
            .join("0123456789abcdef0123456789abcdef.part");
        std::fs::write(&orphan, b"x").unwrap();
        let long_ago = SystemTime::now() - 2 * MAX_IDLE;
        for path in [
            part_path(dir.path(), &idle).unwrap(),
            dir.path().join(document(&idle)),
            orphan.clone(),
        ] {
            std::fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(long_ago)
                .unwrap();
        }
        assert_eq!(sweep(dir.path(), MAX_IDLE).unwrap(), 2);
        assert_eq!(read(&store, dir.path(), &idle).unwrap(), None);
        assert!(!dir.path().join(document(&idle)).exists());
        assert!(!orphan.exists());
        assert!(read(&store, dir.path(), &fresh).unwrap().is_some());
    }

    #[test]
    fn an_upload_being_sent_to_is_not_idle_however_old() {
        let dir = tempfile::tempdir().unwrap();
        let store = DocumentStore::new(dir.path().to_path_buf());
        let id = create(&store, dir.path(), &upload(5)).unwrap();
        // Started long ago; the last chunk arrived just now.
        std::fs::File::options()
            .write(true)
            .open(dir.path().join(document(&id)))
            .unwrap()
            .set_modified(SystemTime::now() - 2 * MAX_IDLE)
            .unwrap();
        assert_eq!(sweep(dir.path(), MAX_IDLE).unwrap(), 0);
        assert!(read(&store, dir.path(), &id).unwrap().is_some());
    }

    #[test]
    fn no_uploads_directory_is_nothing_to_sweep() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(sweep(dir.path(), MAX_IDLE).unwrap(), 0);
    }
}
