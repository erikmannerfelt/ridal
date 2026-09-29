//! An append-only history: JSON Lines, one record per line (#214).
//!
//! ```text
//! audit.jsonl      the current log
//! audit.1.jsonl    the previous one, once the current one has filled up
//! ```
//!
//! What both audit logs are written as, the project's catalog history
//! ([`super::audit`]) and the site's ledger ([`crate::site::audit`]).
//!
//! Appended to and never rewritten. An append costs the same however long
//! the history is, and an interrupted one can damage only the line it was
//! writing, which [`read`] skips; the next append starts on a fresh line.
//! When the current file passes [`MAX_BYTES`] it is renamed to the previous
//! file, replacing the one before, so a history is bounded at about twice
//! that. A rename loses nothing: an append that opened the file just before
//! it lands in the renamed file, which is still read.
//!
//! One `write_all` of one line to a file opened for appending, so concurrent
//! writers -- the server and the CLI -- interleave whole lines rather than
//! mixing them.

use std::io::{Read, Seek, Write};
use std::path::Path;

use serde::de::DeserializeOwned;
use serde::Serialize;

use super::store::DocumentStore;

/// The current log, relative to the store's root.
pub const FILE: &str = "audit.jsonl";

/// The previous log, kept when the current one is rotated.
pub const PREVIOUS_FILE: &str = "audit.1.jsonl";

/// The size at which the current log is rotated. An entry is a couple of
/// hundred bytes, so this keeps something like the last 10 000-20 000.
pub const MAX_BYTES: u64 = 2 * 1024 * 1024;

/// Every record in the store's history, oldest first.
///
/// A line that does not parse is skipped rather than failing the read: the
/// one way it arises in normal operation is an append interrupted part-way,
/// and one lost entry should not hide every other.
pub fn read<T: DeserializeOwned>(store: &DocumentStore) -> std::io::Result<Vec<T>> {
    let mut entries = Vec::new();
    for name in [PREVIOUS_FILE, FILE] {
        let text = match std::fs::read_to_string(store.root().join(name)) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        entries.extend(
            text.lines()
                .filter_map(|line| serde_json::from_str::<T>(line).ok()),
        );
    }
    Ok(entries)
}

/// Append one record, rotating the log first if it has filled up.
pub fn append<T: Serialize>(store: &DocumentStore, entry: &T) -> std::io::Result<()> {
    std::fs::create_dir_all(store.root())?;
    let path = store.root().join(FILE);
    rotate_if_full(&path, &store.root().join(PREVIOUS_FILE))?;
    let mut line = serde_json::to_string(entry).map_err(std::io::Error::other)?;
    line.push('\n');
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        .open(&path)?;
    // A previous append interrupted mid-line would otherwise swallow this
    // entry into the same unparsable line.
    if file.metadata()?.len() > 0 {
        let mut last = [0u8; 1];
        file.seek(std::io::SeekFrom::End(-1))?;
        file.read_exact(&mut last)?;
        if last[0] != b'\n' {
            line.insert(0, '\n');
        }
    }
    file.write_all(line.as_bytes())
}

fn rotate_if_full(path: &Path, previous: &Path) -> std::io::Result<()> {
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.len() >= MAX_BYTES => std::fs::rename(path, previous),
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq, Serialize, serde::Deserialize)]
    struct Record {
        n: u32,
    }

    fn store() -> (tempfile::TempDir, DocumentStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = DocumentStore::new(dir.path().to_path_buf());
        (dir, store)
    }

    fn numbers(store: &DocumentStore) -> Vec<u32> {
        read::<Record>(store)
            .unwrap()
            .into_iter()
            .map(|record| record.n)
            .collect()
    }

    #[test]
    fn no_history_reads_as_empty() {
        let (_dir, store) = store();
        assert!(numbers(&store).is_empty());
    }

    #[test]
    fn records_accumulate_one_per_line() {
        let (dir, store) = store();
        append(&store, &Record { n: 1 }).unwrap();
        append(&store, &Record { n: 2 }).unwrap();
        assert_eq!(numbers(&store), [1, 2]);
        let text = std::fs::read_to_string(dir.path().join(FILE)).unwrap();
        assert_eq!(text.lines().count(), 2);
    }

    #[test]
    fn a_torn_line_costs_only_itself() {
        let (dir, store) = store();
        append(&store, &Record { n: 1 }).unwrap();
        // An append interrupted part-way through its line.
        std::fs::OpenOptions::new()
            .append(true)
            .open(dir.path().join(FILE))
            .unwrap()
            .write_all(br#"{"n":"#)
            .unwrap();
        append(&store, &Record { n: 2 }).unwrap();
        assert_eq!(numbers(&store), [1, 2]);
    }

    #[test]
    fn a_full_log_is_rotated_and_still_read() {
        let (dir, store) = store();
        append(&store, &Record { n: 1 }).unwrap();
        let padding = format!("{}\n", "x".repeat(MAX_BYTES as usize));
        std::fs::OpenOptions::new()
            .append(true)
            .open(dir.path().join(FILE))
            .unwrap()
            .write_all(padding.as_bytes())
            .unwrap();

        append(&store, &Record { n: 2 }).unwrap();
        assert!(dir.path().join(PREVIOUS_FILE).is_file());
        let current = std::fs::read_to_string(dir.path().join(FILE)).unwrap();
        assert_eq!(current.lines().count(), 1);
        assert_eq!(numbers(&store), [1, 2]);
    }
}
