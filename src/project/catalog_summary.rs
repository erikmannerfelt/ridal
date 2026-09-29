//! How many radargrams a project's catalog holds (#214).
//!
//! ```text
//! catalog-summary.json
//! ```
//!
//! The site landing lists projects without opening them: building a
//! project's catalog means inspecting every radargram header, which is
//! exactly the work the lazy per-project loading exists to avoid. So when a
//! catalog *is* scanned -- on open, upload, replace, removal or ignore --
//! its size is written here, and the landing reads it instead.
//!
//! Derived data, and regenerable: delete the file and open the project
//! again. A project never scanned has no file, and the landing says so
//! rather than inventing a zero.

#![cfg_attr(
    not(feature = "server"),
    allow(
        dead_code,
        reason = "written by the server when it scans a catalog; a CLI-only \
                  build still needs the type"
    )
)]

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::store::{DocumentStore, Expectation, StoreError};

/// The document's path, relative to the project's data directory.
pub const FILE: &str = "catalog-summary.json";

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Summary {
    /// The number of catalog entries, unlisted ones included.
    #[serde(default)]
    pub radargrams: usize,
}

#[derive(Debug)]
pub enum SummaryError {
    Store(StoreError),
    Malformed { path: PathBuf, message: String },
}

impl std::fmt::Display for SummaryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SummaryError::Store(e) => write!(f, "{e}"),
            SummaryError::Malformed { path, message } => write!(
                f,
                "{} is not a valid catalog summary: {message}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for SummaryError {}

impl From<StoreError> for SummaryError {
    fn from(e: StoreError) -> Self {
        SummaryError::Store(e)
    }
}

fn path_of() -> PathBuf {
    PathBuf::from(FILE)
}

/// Read a project's summary, or `None` when it has never been scanned.
///
/// Lenient: a malformed file means "not scanned", not a failed request. The
/// landing page is a convenience, and one broken file must not take it down.
pub fn read(store: &DocumentStore) -> Option<Summary> {
    let stored = store.read(&path_of()).ok()??;
    serde_json::from_str(&stored.text).ok()
}

/// Record the catalog's size, then read it back.
///
/// Returns the re-read value, so a caller and a later reader see the same
/// thing including any serializer-induced difference.
pub fn write(store: &DocumentStore, summary: &Summary) -> Result<Summary, SummaryError> {
    let mut text = serde_json::to_string_pretty(summary).map_err(|e| SummaryError::Malformed {
        path: store.root().join(path_of()),
        message: e.to_string(),
    })?;
    text.push('\n');
    store.write(&path_of(), &text, &Expectation::Any)?;
    read(store).ok_or_else(|| SummaryError::Malformed {
        path: store.root().join(path_of()),
        message: "the summary could not be read back".to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, DocumentStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = DocumentStore::new(dir.path().to_path_buf());
        (dir, store)
    }

    #[test]
    fn a_project_with_no_summary_reads_as_none() {
        let (_dir, store) = store();
        assert!(read(&store).is_none());
    }

    #[test]
    fn a_written_summary_reads_back_the_same() {
        let (_dir, store) = store();
        let written = write(&store, &Summary { radargrams: 7 }).unwrap();
        assert_eq!(written.radargrams, 7);
        assert_eq!(read(&store).unwrap().radargrams, 7);
    }

    #[test]
    fn a_malformed_summary_reads_as_unscanned() {
        let (_dir, store) = store();
        store
            .write(&path_of(), "{ not json", &Expectation::Any)
            .unwrap();
        assert!(read(&store).is_none());
    }
}
