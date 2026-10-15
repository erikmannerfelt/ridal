//! Reading and writing level 1 interpretations inside a project.
//!
//! One document per user per radargram:
//!
//! ```text
//! interpretations/<radargram-id>/<user>.gprinterp.json
//! ```
//!
//! Splitting by user means two people interpreting the same radargram never
//! write to the same file, so their edits cannot conflict at all -- the only
//! conflicts left are one user in two browser tabs, which the store's
//! version check already handles. It also makes "show only my picks" a file
//! selection rather than a filter, and keeps each document a plain gprinterp
//! file that can be handed to any other tool unchanged.
//!
//! Both path components are validated slug types rather than strings off the
//! wire, so a request cannot address anything outside its own directory.

#![cfg_attr(
    not(feature = "server"),
    allow(
        dead_code,
        reason = "the write half of the project API is reached through the \
                  server's HTTP routes; a CLI-only build still needs the \
                  types to read and inspect a project"
    )
)]

use std::path::PathBuf;

use gprinterp::Document;

use crate::identity::{RadargramId, UserId};
use crate::project::store::{DocumentStore, Expectation, StoreError, Version};

/// Filename suffix, chosen so the gprinterp extension stays visible.
pub const SUFFIX: &str = ".gprinterp.json";

/// The directory under `interpretations/` that holds removed radargrams'
/// picks. See [`archive_all`].
const ARCHIVE_DIR: &str = "_archived";

/// The stored form of one user's interpretation of one radargram.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredInterpretation {
    pub document: Document,
    pub version: Version,
    /// Size of the file as stored.
    pub bytes: usize,
}

#[derive(Debug)]
pub enum InterpretationError {
    Store(StoreError),
    /// The file on disk is not a parseable gprinterp document.
    Malformed {
        path: PathBuf,
        message: String,
    },
    /// The document's `key` names a different radargram than the path it is
    /// being stored under.
    KeyMismatch {
        expected: String,
        found: String,
    },
}

impl std::fmt::Display for InterpretationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InterpretationError::Store(e) => write!(f, "{e}"),
            InterpretationError::Malformed { path, message } => write!(
                f,
                "{} is not a valid gprinterp document: {message}",
                path.display()
            ),
            InterpretationError::KeyMismatch { expected, found } => write!(
                f,
                "the interpretation names radargram '{found}', but it is being saved \
                 for '{expected}'"
            ),
        }
    }
}

impl std::error::Error for InterpretationError {}

impl From<StoreError> for InterpretationError {
    fn from(e: StoreError) -> Self {
        InterpretationError::Store(e)
    }
}

fn directory_of(radargram: &RadargramId) -> PathBuf {
    PathBuf::from(crate::project::INTERPRETATIONS_DIR).join(radargram.as_str())
}

/// Where a live interpretation is, relative to the store.
pub fn path_of(radargram: &RadargramId, user: &UserId) -> PathBuf {
    directory_of(radargram).join(format!("{}{SUFFIX}", user.as_str()))
}

/// Users who have an interpretation of `radargram`.
pub fn list_users(
    store: &DocumentStore,
    radargram: &RadargramId,
) -> Result<Vec<String>, InterpretationError> {
    Ok(store.list_stems(&directory_of(radargram), SUFFIX)?)
}

/// Users who have an interpretation, split into the readable and the not.
///
/// A stem is a filename the server wrote, but [`UserId`] rejects anything
/// outside its charset rather than sanitising, so a file placed by hand or by
/// a converter that did not know the rules is unreadable. Callers used to
/// handle that four different ways -- four silent `continue`s and two `?`s
/// that turned one bad filename into a 500 for the whole request, hiding every
/// good file behind it (#213).
///
/// Returning both halves makes the honest thing the easy thing: skip what
/// cannot be read so one bad name cannot hide the rest, **and** report it, so
/// a consensus computed over fewer contributors than exist can say so. Silence
/// is the worse failure of the two -- a median over 9 of 10 contributors
/// presented as *the* consensus is wrong in a way nobody can see.
pub fn list_users_checked(
    store: &DocumentStore,
    radargram: &RadargramId,
) -> Result<(Vec<UserId>, Vec<String>), InterpretationError> {
    let mut readable = Vec::new();
    let mut unreadable = Vec::new();
    for stem in list_users(store, radargram)? {
        match UserId::new(stem.as_str()) {
            Ok(user) => readable.push(user),
            Err(_) => unreadable.push(stem),
        }
    }
    Ok((readable, unreadable))
}

/// Read one user's interpretation, or `None` if they have not made one.
pub fn read(
    store: &DocumentStore,
    radargram: &RadargramId,
    user: &UserId,
) -> Result<Option<StoredInterpretation>, InterpretationError> {
    read_at(store, &path_of(radargram, user))
}

fn read_at(
    store: &DocumentStore,
    relative: &std::path::Path,
) -> Result<Option<StoredInterpretation>, InterpretationError> {
    let Some(stored) = store.read(relative)? else {
        return Ok(None);
    };
    let document =
        Document::from_json(&stored.text).map_err(|e| InterpretationError::Malformed {
            path: store.root().join(relative),
            message: e.to_string(),
        })?;
    Ok(Some(StoredInterpretation {
        document,
        version: stored.version,
        bytes: stored.text.len(),
    }))
}

/// Every directory of live interpretations, named by radargram id.
///
/// Read from the disk rather than the catalog, which is the point: a
/// directory whose radargram is no longer served still holds picks, and the
/// pick manager has to be able to say so (#178). Names that are not a valid
/// [`RadargramId`] are returned in the second list, as
/// [`list_users_checked`] does for file names (#213).
pub fn list_radargrams(
    store: &DocumentStore,
) -> Result<(Vec<RadargramId>, Vec<String>), InterpretationError> {
    let mut readable = Vec::new();
    let mut unreadable = Vec::new();
    for name in store.list_subdirectories(&PathBuf::from(crate::project::INTERPRETATIONS_DIR))? {
        if name == ARCHIVE_DIR {
            continue;
        }
        match RadargramId::new(name.as_str()) {
            Ok(id) => readable.push(id),
            Err(_) => unreadable.push(name),
        }
    }
    Ok((readable, unreadable))
}

/// One archived interpretation: whose, of what, and from which removal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Archived {
    pub radargram: RadargramId,
    /// The removal's directory name: the removal timestamp with `:` written
    /// as `-`, and a `-2`, `-3`... suffix when removals shared a second.
    pub removal: String,
    pub user: UserId,
}

/// Every interpretation in the archive (#162), and the paths, relative to
/// the archive, of files that could not be named.
pub fn list_archived(
    store: &DocumentStore,
) -> Result<(Vec<Archived>, Vec<String>), InterpretationError> {
    let base = PathBuf::from(crate::project::INTERPRETATIONS_DIR).join(ARCHIVE_DIR);
    let mut archived = Vec::new();
    let mut unreadable = Vec::new();
    for radargram_name in store.list_subdirectories(&base)? {
        let Ok(radargram) = RadargramId::new(radargram_name.as_str()) else {
            unreadable.push(radargram_name);
            continue;
        };
        let radargram_dir = base.join(&radargram_name);
        for removal in store.list_subdirectories(&radargram_dir)? {
            for stem in store.list_stems(&radargram_dir.join(&removal), SUFFIX)? {
                match UserId::new(stem.as_str()) {
                    Ok(user) => archived.push(Archived {
                        radargram: radargram.clone(),
                        removal: removal.clone(),
                        user,
                    }),
                    Err(_) => unreadable.push(format!("{radargram_name}/{removal}/{stem}{SUFFIX}")),
                }
            }
        }
    }
    Ok((archived, unreadable))
}

/// Read one archived interpretation, or `None` if it is not there.
pub fn read_archived(
    store: &DocumentStore,
    archived: &Archived,
) -> Result<Option<StoredInterpretation>, InterpretationError> {
    read_at(store, &archived_path(archived))
}

/// Where an archived interpretation is, relative to the store.
pub fn archived_path(archived: &Archived) -> PathBuf {
    PathBuf::from(crate::project::INTERPRETATIONS_DIR)
        .join(ARCHIVE_DIR)
        .join(archived.radargram.as_str())
        .join(&archived.removal)
        .join(format!("{}{SUFFIX}", archived.user.as_str()))
}

/// Write one user's interpretation.
///
/// The document's `key` must name the radargram it is being stored under.
/// Storing a mismatched one would put an interpretation of line A in line
/// B's directory, where every later read would apply it to the wrong data
/// and no downstream check could notice.
pub fn write(
    store: &DocumentStore,
    radargram: &RadargramId,
    user: &UserId,
    document: &Document,
    expected: &Expectation,
) -> Result<Version, InterpretationError> {
    if document.key != radargram.as_str() {
        return Err(InterpretationError::KeyMismatch {
            expected: radargram.as_str().to_string(),
            found: document.key.clone(),
        });
    }
    let text = document
        .to_json_pretty()
        .map_err(|e| InterpretationError::Malformed {
            path: path_of(radargram, user),
            message: e.to_string(),
        })?;
    Ok(store.write(&path_of(radargram, user), &text, expected)?)
}

/// Where a removed radargram's interpretations are kept.
///
/// ```text
/// interpretations/_archived/<radargram>/<removed-at>/<user>.gprinterp.json
/// ```
///
/// `_archived` cannot collide with a radargram directory: a `RadargramId`
/// is a validated slug and the leading underscore is not one a slug can
/// start with. The timestamp layer means removing, re-adding and removing
/// again keeps both sets rather than the second quietly replacing the
/// first.
fn archive_directory(radargram: &RadargramId, at: &str) -> PathBuf {
    PathBuf::from(crate::project::INTERPRETATIONS_DIR)
        .join(ARCHIVE_DIR)
        .join(radargram.as_str())
        // ':' is not a filename character on Windows, and an RFC 3339
        // timestamp is full of them.
        .join(at.replace(':', "-"))
}

/// The first archive directory for this removal that does not already
/// exist.
///
/// A timestamp is not a uniqueness guarantee. Remove, re-add and remove
/// again inside one second -- a script, or two operators -- and both
/// removals name the same directory, and the second write would land on top
/// of the first. The set that would be lost is the older one, which is
/// exactly the one nobody is watching.
fn free_archive_directory(
    store: &DocumentStore,
    radargram: &RadargramId,
    at: &str,
) -> Result<PathBuf, InterpretationError> {
    let base = archive_directory(radargram, at);
    if store.list_stems(&base, SUFFIX)?.is_empty() {
        return Ok(base);
    }
    for n in 2..100 {
        let candidate = base.with_file_name(format!(
            "{}-{n}",
            base.file_name().unwrap_or_default().to_string_lossy()
        ));
        if store.list_stems(&candidate, SUFFIX)?.is_empty() {
            return Ok(candidate);
        }
    }
    Err(InterpretationError::Store(StoreError::Io {
        path: base,
        source: std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "a hundred archives of this radargram share one timestamp",
        ),
    }))
}

/// How many interpretations this id has in the archive, across every
/// removal.
///
/// Read when a radargram is added, so that adding one whose id was removed
/// before says so. Re-adding an id is a legitimate thing to do — a
/// reprocessed line arrives under the id it always had — and refusing it
/// would be wrong. What would also be wrong is doing it in silence: the
/// picks in the archive were drawn on whatever held this id last, which may
/// or may not be what is arriving now, and the operator is the only one who
/// knows which. This is what lets the add report it.
pub fn count_archived(
    store: &DocumentStore,
    radargram: &RadargramId,
) -> Result<usize, InterpretationError> {
    let base = PathBuf::from(crate::project::INTERPRETATIONS_DIR)
        .join(ARCHIVE_DIR)
        .join(radargram.as_str());
    let mut total = 0;
    for removal in store.list_subdirectories(&base)? {
        total += store.list_stems(&base.join(&removal), SUFFIX)?.len();
    }
    Ok(total)
}

/// Copy one user's interpretation into the archive, leaving the original
/// in place.
///
/// A *copy*, unlike [`archive_all`]: this is taken before a document is
/// overwritten rather than before it disappears, so the thing being
/// preserved is the version about to be replaced while the live document
/// carries on existing.
///
/// What it preserves is the coordinates as somebody actually drew them.
/// Promoting a carried interpretation writes approximate coordinates over
/// exact ones — that is what promoting *is* — and #148's objection to
/// migration is that doing so is irreversible. This is what makes it
/// reversible, so the objection stops applying.
///
/// Returns the archive path, or `None` when there was nothing to archive.
pub fn archive_one(
    store: &DocumentStore,
    radargram: &RadargramId,
    user: &UserId,
    at: &str,
) -> Result<Option<PathBuf>, InterpretationError> {
    let source = path_of(radargram, user);
    let Some(stored) = store.read(&source)? else {
        return Ok(None);
    };
    let destination =
        free_archive_directory(store, radargram, at)?.join(format!("{}{SUFFIX}", user.as_str()));
    store.write(&destination, &stored.text, &Expectation::Any)?;
    Ok(Some(destination))
}

/// Move every interpretation of `radargram` into the archive, returning how
/// many moved.
///
/// What [`archive_all`] did, and what it could not.
///
/// `skipped` names files left in place because their filename is not a valid
/// [`UserId`] and the archive path is built from one (#213). That matters more
/// here than elsewhere: the whole point of archiving is that nothing authored
/// is destroyed and no orphan is left to reattach to a different radargram
/// under the same id, so a file quietly left behind defeats the purpose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveReport {
    pub moved: usize,
    pub skipped: Vec<String>,
}

/// Archived rather than deleted, and not only to be careful with data. The
/// hazard is that someone removes a radargram, someone else later adds a
/// *different* file under the same id, and the orphaned picks silently
/// reattach to data they were never drawn on. `check_identity` would catch
/// the revision mismatch, but only when something asks for a level 2
/// export — the viewer would simply draw them.
///
/// Nothing authored is destroyed, which is the same rule #131 settled for a
/// departed user's picks: those are attributed scientific data and the
/// account going away must not take them.
pub fn archive_all(
    store: &DocumentStore,
    radargram: &RadargramId,
    at: &str,
) -> Result<ArchiveReport, InterpretationError> {
    // Skip and report (#213). An unreadable filename is left where it is
    // rather than archived, since the archive path is built from a `UserId`.
    let (users, unreadable) = list_users_checked(store, radargram)?;
    if users.is_empty() {
        return Ok(ArchiveReport {
            moved: 0,
            skipped: unreadable,
        });
    }
    let destination = free_archive_directory(store, radargram, at)?;
    let mut moved = 0;
    for user_id in users {
        let source = path_of(radargram, &user_id);
        let Some(stored) = store.read(&source)? else {
            continue;
        };
        // Written before the original is removed, so a failure between the
        // two leaves a copy rather than nothing. A duplicate is recoverable
        // and an absence is not.
        store.write(
            &destination.join(format!("{}{SUFFIX}", user_id.as_str())),
            &stored.text,
            &Expectation::Any,
        )?;
        store.remove(&source, &Expectation::Any)?;
        moved += 1;
    }
    Ok(ArchiveReport {
        moved,
        skipped: unreadable,
    })
}

/// Move one user's interpretation into the archive (#178), returning where
/// it went, or `None` when there was nothing to move.
///
/// What an administrator does to someone else's picks instead of deleting
/// them: the document leaves the radargram, so it is no longer drawn,
/// counted or exported, and stays attributed in the archive, where only a
/// purge (#162) destroys it. The same rule as [`archive_all`] for a removed
/// radargram, applied to one document.
///
/// `expected` is checked against the document before anything is copied,
/// and the original is removed only at the version that was copied, so the
/// archive holds exactly what left the radargram.
pub fn archive_and_remove(
    store: &DocumentStore,
    radargram: &RadargramId,
    user: &UserId,
    at: &str,
    expected: &Expectation,
) -> Result<Option<PathBuf>, InterpretationError> {
    let source = path_of(radargram, user);
    let Some(stored) = store.read(&source)? else {
        return Ok(None);
    };
    if let Expectation::Version(wanted) = expected {
        if *wanted != stored.version {
            return Err(InterpretationError::Store(StoreError::Conflict {
                expected: expected.clone(),
                actual: Some(stored.version),
            }));
        }
    }
    let destination =
        free_archive_directory(store, radargram, at)?.join(format!("{}{SUFFIX}", user.as_str()));
    // Copied before the original is removed, as in `archive_all`: a failure
    // between the two leaves a duplicate rather than nothing.
    store.write(&destination, &stored.text, &Expectation::Any)?;
    store.remove(&source, &Expectation::Version(stored.version))?;
    Ok(Some(destination))
}

/// Destroy archived interpretations (#162): every document from one removal
/// of `radargram`, or only `user`'s. Returns whose were destroyed, empty
/// when the removal does not exist.
///
/// `removal` is matched against the removals that exist rather than joined
/// into a path, so a name off the wire can only ever select one of them.
/// Files whose names are not a valid [`UserId`] are left in place, and so is
/// the directory holding them.
pub fn purge_archived(
    store: &DocumentStore,
    radargram: &RadargramId,
    removal: &str,
    user: Option<&UserId>,
) -> Result<Vec<UserId>, InterpretationError> {
    let base = PathBuf::from(crate::project::INTERPRETATIONS_DIR)
        .join(ARCHIVE_DIR)
        .join(radargram.as_str());
    let Some(removal) = store
        .list_subdirectories(&base)?
        .into_iter()
        .find(|existing| existing == removal)
    else {
        return Ok(Vec::new());
    };
    let directory = base.join(&removal);
    let mut purged = Vec::new();
    for stem in store.list_stems(&directory, SUFFIX)? {
        let Ok(owner) = UserId::new(stem.as_str()) else {
            continue;
        };
        if user.is_some_and(|wanted| *wanted != owner) {
            continue;
        }
        if store.remove(
            &directory.join(format!("{stem}{SUFFIX}")),
            &Expectation::Any,
        )? {
            purged.push(owner);
        }
    }
    store.remove_empty_dir(&directory)?;
    store.remove_empty_dir(&base)?;
    Ok(purged)
}

/// What [`restore_archived`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Restore {
    /// The document is back on its radargram, at this version.
    Done(Version),
    /// There is no such archived document.
    NotArchived,
    /// The user already has an interpretation of the radargram, so nothing
    /// was moved: two documents are not merged into one.
    Occupied,
}

/// Move one archived interpretation back onto its radargram (#435).
///
/// The document is restored byte for byte, still naming the revision it was
/// drawn on, so when that is not the current one it is carried like any
/// other stale interpretation and adopted by its owner as usual. Written
/// only where there is nothing yet, then removed from the archive: a
/// failure between the two leaves a duplicate rather than nothing.
pub fn restore_archived(
    store: &DocumentStore,
    archived: &Archived,
) -> Result<Restore, InterpretationError> {
    let source = archived_path(archived);
    // Parsed as well as read, so a document that would not load is not put
    // where the viewer would trip over it.
    let Some(stored) = store.read(&source)? else {
        return Ok(Restore::NotArchived);
    };
    Document::from_json(&stored.text).map_err(|e| InterpretationError::Malformed {
        path: store.root().join(&source),
        message: e.to_string(),
    })?;
    let destination = path_of(&archived.radargram, &archived.user);
    let version = match store.write(&destination, &stored.text, &Expectation::Absent) {
        Ok(version) => version,
        Err(StoreError::Conflict { .. }) => return Ok(Restore::Occupied),
        Err(e) => return Err(e.into()),
    };
    store.remove(&source, &Expectation::Version(stored.version))?;
    let removal = source.parent().expect("an archived path has a parent");
    store.remove_empty_dir(removal)?;
    store.remove_empty_dir(removal.parent().expect("a removal has a parent"))?;
    Ok(Restore::Done(version))
}

/// Delete one user's interpretation. Returns whether it existed.
pub fn remove(
    store: &DocumentStore,
    radargram: &RadargramId,
    user: &UserId,
    expected: &Expectation,
) -> Result<bool, InterpretationError> {
    Ok(store.remove(&path_of(radargram, user), expected)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::DEFAULT_USER;
    use crate::project::Project;

    fn project() -> (tempfile::TempDir, Project) {
        let dir = tempfile::tempdir().unwrap();
        let project = Project::init(dir.path(), None).unwrap();
        (dir, project)
    }

    fn document(key: &str) -> Document {
        serde_json::from_value(serde_json::json!({
            "key": key,
            "features": [{
                "type": "Feature",
                "geometry": {"type": "LineString", "coordinates": [[10.0, 200.0], [90.0, 210.0]]},
                "properties": {"id": "f-0001", "label": "bed"}
            }]
        }))
        .unwrap()
    }

    fn ids(radargram: &str, user: &str) -> (RadargramId, UserId) {
        (
            RadargramId::new(radargram).unwrap(),
            UserId::new(user).unwrap(),
        )
    }

    #[test]
    fn an_interpretation_round_trips_through_the_store() {
        let (_dir, project) = project();
        let (radargram, user) = ids("dronbreen-0237", DEFAULT_USER);
        let document = document("dronbreen-0237");

        let version = write(
            project.documents(),
            &radargram,
            &user,
            &document,
            &Expectation::Absent,
        )
        .unwrap();

        let stored = read(project.documents(), &radargram, &user)
            .unwrap()
            .unwrap();
        assert_eq!(stored.document, document);
        assert_eq!(stored.version, version);
    }

    #[test]
    fn the_archive_lists_by_removal_and_is_not_a_live_directory() {
        let (_dir, project) = project();
        let store = project.documents();
        let (radargram, erik) = ids("dronbreen-0237", "erik");
        let other = RadargramId::new("dronbreen-0238").unwrap();
        for id in [&radargram, &other] {
            write(
                store,
                id,
                &erik,
                &document(id.as_str()),
                &Expectation::Absent,
            )
            .unwrap();
        }
        archive_all(store, &radargram, "2026-10-14T10:00:00Z").unwrap();

        // The emptied directory may stay behind; the archive is never
        // listed as a radargram, valid id or not.
        let (live, bad) = list_radargrams(store).unwrap();
        assert!(live.contains(&other));
        assert!(bad.is_empty(), "{bad:?}");

        let (archived, bad) = list_archived(store).unwrap();
        assert!(bad.is_empty());
        assert_eq!(
            archived,
            vec![Archived {
                radargram: radargram.clone(),
                removal: "2026-10-14T10-00-00Z".to_string(),
                user: erik,
            }]
        );
        let stored = read_archived(store, &archived[0]).unwrap().unwrap();
        assert_eq!(stored.document.key, "dronbreen-0237");
        assert!(stored.bytes > 0);
    }

    #[test]
    fn archiving_one_moves_it_and_purging_destroys_only_what_was_named() {
        let (_dir, project) = project();
        let store = project.documents();
        let (radargram, erik) = ids("dronbreen-0237", "erik");
        let anna = UserId::new("anna").unwrap();
        let version = write(
            store,
            &radargram,
            &erik,
            &document("dronbreen-0237"),
            &Expectation::Absent,
        )
        .unwrap();
        write(
            store,
            &radargram,
            &anna,
            &document("dronbreen-0237"),
            &Expectation::Absent,
        )
        .unwrap();

        // A stale version refuses and moves nothing.
        let stale = Expectation::Version(Version::from_header("not-it"));
        assert!(matches!(
            archive_and_remove(store, &radargram, &erik, "2026-10-14T10:00:00Z", &stale),
            Err(InterpretationError::Store(StoreError::Conflict { .. }))
        ));
        assert!(read(store, &radargram, &erik).unwrap().is_some());

        let at = "2026-10-14T10:00:00Z";
        let moved =
            archive_and_remove(store, &radargram, &erik, at, &Expectation::Version(version))
                .unwrap();
        assert!(moved.is_some());
        archive_and_remove(store, &radargram, &anna, at, &Expectation::Any).unwrap();
        assert!(read(store, &radargram, &erik).unwrap().is_none());
        let (archived, _) = list_archived(store).unwrap();
        // One removal directory each: two archive events in one second do
        // not share a directory.
        let removals: Vec<_> = archived.iter().map(|a| a.removal.as_str()).collect();
        assert_eq!(
            removals,
            vec!["2026-10-14T10-00-00Z", "2026-10-14T10-00-00Z-2"]
        );

        // Only a removal that exists can be named.
        assert!(purge_archived(store, &radargram, "../..", None)
            .unwrap()
            .is_empty());
        assert_eq!(
            purge_archived(store, &radargram, "2026-10-14T10-00-00Z", Some(&anna)).unwrap(),
            Vec::<UserId>::new()
        );
        assert_eq!(
            purge_archived(store, &radargram, "2026-10-14T10-00-00Z", None).unwrap(),
            vec![erik]
        );
        let (archived, _) = list_archived(store).unwrap();
        assert_eq!(archived.len(), 1);
        assert_eq!(archived[0].user, anna);
        // The emptied removal directory is gone; the other one stays.
        assert_eq!(
            store
                .list_subdirectories(&PathBuf::from("interpretations/_archived/dronbreen-0237"))
                .unwrap(),
            vec!["2026-10-14T10-00-00Z-2"]
        );
    }

    #[test]
    fn it_lands_at_the_documented_path() {
        // The layout is part of the contract: other tools are expected to
        // read these files directly.
        let (_dir, project) = project();
        let (radargram, user) = ids("dronbreen-0237", DEFAULT_USER);
        write(
            project.documents(),
            &radargram,
            &user,
            &document("dronbreen-0237"),
            &Expectation::Absent,
        )
        .unwrap();

        assert!(project
            .data_dir()
            .join("interpretations/dronbreen-0237/default.gprinterp.json")
            .is_file());
    }

    #[test]
    fn two_users_of_one_radargram_never_share_a_file() {
        let (_dir, project) = project();
        let (radargram, erik) = ids("dronbreen-0237", "erik");
        let student = UserId::new("student-a").unwrap();

        write(
            project.documents(),
            &radargram,
            &erik,
            &document("dronbreen-0237"),
            &Expectation::Absent,
        )
        .unwrap();
        // Absent, not Any: if these shared a file this would be a conflict.
        write(
            project.documents(),
            &radargram,
            &student,
            &document("dronbreen-0237"),
            &Expectation::Absent,
        )
        .unwrap();

        assert_eq!(
            list_users(project.documents(), &radargram).unwrap(),
            vec!["erik", "student-a"]
        );
    }

    #[test]
    fn a_document_for_a_different_radargram_is_refused() {
        let (_dir, project) = project();
        let (radargram, user) = ids("dronbreen-0237", DEFAULT_USER);

        let error = write(
            project.documents(),
            &radargram,
            &user,
            &document("kroppbreen-01"),
            &Expectation::Absent,
        )
        .unwrap_err();
        assert!(
            matches!(error, InterpretationError::KeyMismatch { .. }),
            "{error}"
        );
        assert!(read(project.documents(), &radargram, &user)
            .unwrap()
            .is_none());
    }

    #[test]
    fn a_second_save_from_a_stale_tab_is_refused() {
        let (_dir, project) = project();
        let (radargram, user) = ids("dronbreen-0237", DEFAULT_USER);
        let first = write(
            project.documents(),
            &radargram,
            &user,
            &document("dronbreen-0237"),
            &Expectation::Absent,
        )
        .unwrap();

        let mut edited = document("dronbreen-0237");
        edited.features.clear();
        write(
            project.documents(),
            &radargram,
            &user,
            &edited,
            &Expectation::Version(first.clone()),
        )
        .unwrap();

        let error = write(
            project.documents(),
            &radargram,
            &user,
            &document("dronbreen-0237"),
            &Expectation::Version(first),
        )
        .unwrap_err();
        assert!(matches!(
            error,
            InterpretationError::Store(StoreError::Conflict { .. })
        ));
    }

    #[test]
    fn unknown_fields_survive_a_save_and_reload() {
        // The gprinterp round-trip requirement has to hold through storage,
        // not just through the parser: a field this Ridal does not model
        // must still be there after the GUI saves over it.
        let (_dir, project) = project();
        let (radargram, user) = ids("dronbreen-0237", DEFAULT_USER);
        let document: Document = serde_json::from_value(serde_json::json!({
            "key": "dronbreen-0237",
            "features": [],
            "something_from_a_future_version": {"nested": [1, 2, 3]}
        }))
        .unwrap();

        write(
            project.documents(),
            &radargram,
            &user,
            &document,
            &Expectation::Absent,
        )
        .unwrap();
        let stored = read(project.documents(), &radargram, &user)
            .unwrap()
            .unwrap();

        assert_eq!(
            stored.document.extra.get("something_from_a_future_version"),
            document.extra.get("something_from_a_future_version")
        );
    }

    #[test]
    fn listing_a_radargram_nobody_has_interpreted_is_empty() {
        let (_dir, project) = project();
        let radargram = RadargramId::new("untouched").unwrap();
        assert!(list_users(project.documents(), &radargram)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn removing_reports_whether_anything_was_there() {
        let (_dir, project) = project();
        let (radargram, user) = ids("dronbreen-0237", DEFAULT_USER);
        assert!(!remove(project.documents(), &radargram, &user, &Expectation::Any).unwrap());

        write(
            project.documents(),
            &radargram,
            &user,
            &document("dronbreen-0237"),
            &Expectation::Absent,
        )
        .unwrap();
        assert!(remove(project.documents(), &radargram, &user, &Expectation::Any).unwrap());
    }

    #[test]
    fn a_corrupt_document_names_the_file_rather_than_failing_obscurely() {
        let (_dir, project) = project();
        let (radargram, user) = ids("dronbreen-0237", DEFAULT_USER);
        let path = project.data_dir().join("interpretations/dronbreen-0237");
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join("default.gprinterp.json"), "{ not json").unwrap();

        let error = read(project.documents(), &radargram, &user).unwrap_err();
        assert!(
            format!("{error}").contains("default.gprinterp.json"),
            "{error}"
        );
    }
}

#[cfg(test)]
mod archive_tests {
    use super::*;
    use crate::project::Project;

    fn project() -> (tempfile::TempDir, Project) {
        let dir = tempfile::tempdir().unwrap();
        let project = Project::init(dir.path(), None).unwrap();
        (dir, project)
    }

    fn radargram(name: &str) -> RadargramId {
        RadargramId::new(name).unwrap()
    }

    fn user(name: &str) -> UserId {
        UserId::new(name).unwrap()
    }

    fn write_picks(store: &DocumentStore, id: &RadargramId, who: &str) {
        let text = format!(
            r#"{{"schema":"gprinterp","key":"{}","features":[],"note":"{who}"}}"#,
            id.as_str()
        );
        store
            .write(&path_of(id, &user(who)), &text, &Expectation::Any)
            .unwrap();
    }

    #[test]
    fn archiving_moves_every_users_picks_and_leaves_none_behind() {
        // The hazard removal has to avoid: a different file arrives later
        // under the same id and orphaned picks silently reattach to data
        // they were never drawn on.
        let (_dir, project) = project();
        let store = project.documents();
        let id = radargram("line-01");
        write_picks(store, &id, "erik");
        write_picks(store, &id, "student");

        let moved = archive_all(store, &id, "2026-09-13T12:00:00Z")
            .unwrap()
            .moved;
        assert_eq!(moved, 2);
        assert!(
            list_users(store, &id).unwrap().is_empty(),
            "nothing is left to reattach"
        );

        let archived = project
            .data_dir()
            .join("interpretations/_archived/line-01/2026-09-13T12-00-00Z");
        let mut names: Vec<String> = std::fs::read_dir(&archived)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec!["erik.gprinterp.json", "student.gprinterp.json"],
            "nothing authored is destroyed"
        );
        // And the content is the picks, not an empty placeholder.
        assert!(
            std::fs::read_to_string(archived.join("erik.gprinterp.json"))
                .unwrap()
                .contains("\"note\":\"erik\"")
        );
    }

    #[test]
    fn removing_twice_keeps_both_sets_rather_than_the_second_replacing_the_first() {
        // Remove, re-add, remove again. Without the timestamp layer the
        // second archive would overwrite the first, and the picks from
        // before the re-add would be the ones lost -- the older and more
        // easily forgotten of the two.
        let (_dir, project) = project();
        let store = project.documents();
        let id = radargram("line-01");

        write_picks(store, &id, "erik");
        archive_all(store, &id, "2026-09-13T12:00:00Z").unwrap();
        write_picks(store, &id, "erik");
        archive_all(store, &id, "2026-09-14T12:00:00Z").unwrap();

        let archived = project.data_dir().join("interpretations/_archived/line-01");
        let mut stamps: Vec<String> = std::fs::read_dir(&archived)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        stamps.sort();
        assert_eq!(stamps, vec!["2026-09-13T12-00-00Z", "2026-09-14T12-00-00Z"]);
    }

    #[test]
    fn the_archive_count_spans_every_removal_of_an_id() {
        // What an add reads to decide whether the id it is taking carries a
        // history. Counting only the most recent removal would under-report
        // exactly the case the count exists for: an id that has been reused
        // before.
        let (_dir, project) = project();
        let store = project.documents();
        let id = radargram("line-01");

        assert_eq!(count_archived(store, &id).unwrap(), 0, "a fresh id");

        write_picks(store, &id, "erik");
        archive_all(store, &id, "2026-09-13T12:00:00Z").unwrap();
        assert_eq!(count_archived(store, &id).unwrap(), 1);

        write_picks(store, &id, "erik");
        write_picks(store, &id, "student");
        archive_all(store, &id, "2026-09-14T12:00:00Z").unwrap();
        assert_eq!(count_archived(store, &id).unwrap(), 3, "both removals");

        // And it is per id, not per project.
        assert_eq!(count_archived(store, &radargram("line-02")).unwrap(), 0);
    }

    #[test]
    fn a_live_interpretation_is_not_counted_as_an_archived_one() {
        // The count answers "was this id removed with picks on it", so picks
        // that are still attached must not read as history. Sharing a parent
        // directory with `_archived` makes this easy to get wrong.
        let (_dir, project) = project();
        let store = project.documents();
        let id = radargram("line-01");

        write_picks(store, &id, "erik");
        assert_eq!(count_archived(store, &id).unwrap(), 0);
    }

    #[test]
    fn two_removals_in_the_same_second_do_not_overwrite_each_other() {
        // A timestamp is not a uniqueness guarantee. Remove, re-add and
        // remove again inside one second -- a script, or two operators --
        // and both removals name the same directory. The set that would be
        // lost is the older one, which is exactly the one nobody is
        // watching.
        let (_dir, project) = project();
        let store = project.documents();
        let id = radargram("line-01");
        let at = "2026-09-13T12:00:00Z";

        write_picks(store, &id, "erik");
        assert_eq!(archive_all(store, &id, at).unwrap().moved, 1);
        write_picks(store, &id, "student");
        assert_eq!(archive_all(store, &id, at).unwrap().moved, 1);

        let archived = project.data_dir().join("interpretations/_archived/line-01");
        let mut dirs: Vec<String> = std::fs::read_dir(&archived)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        dirs.sort();
        assert_eq!(dirs.len(), 2, "both survived: {dirs:?}");
        // And each holds the picks it was given, rather than one holding
        // both or the later overwriting the earlier.
        assert!(archived.join(&dirs[0]).join("erik.gprinterp.json").exists());
        assert!(archived
            .join(&dirs[1])
            .join("student.gprinterp.json")
            .exists());
    }

    #[test]
    fn archiving_a_radargram_nobody_picked_is_not_an_error() {
        let (_dir, project) = project();
        let report = archive_all(
            project.documents(),
            &radargram("untouched"),
            "2026-09-13T12:00:00Z",
        )
        .unwrap();
        assert_eq!(report.moved, 0);
        assert!(report.skipped.is_empty());
    }

    fn only_archived(store: &DocumentStore) -> Archived {
        let (mut archived, _) = list_archived(store).unwrap();
        assert_eq!(archived.len(), 1);
        archived.remove(0)
    }

    #[test]
    fn restoring_puts_the_document_back_byte_for_byte_and_empties_the_archive() {
        let (_dir, project) = project();
        let store = project.documents();
        let id = radargram("line-01");
        write_picks(store, &id, "erik");
        let before = store.read(&path_of(&id, &user("erik"))).unwrap().unwrap();
        archive_all(store, &id, "2026-09-13T12:00:00Z").unwrap();

        let item = only_archived(store);
        let restored = restore_archived(store, &item).unwrap();

        let after = store.read(&path_of(&id, &user("erik"))).unwrap().unwrap();
        assert_eq!(restored, Restore::Done(after.version.clone()));
        assert_eq!(after.text, before.text);
        assert!(list_archived(store).unwrap().0.is_empty());
        // Nothing is left to show as an empty removal.
        assert!(!project
            .data_dir()
            .join("interpretations/_archived/line-01")
            .exists());
        assert_eq!(
            restore_archived(store, &item).unwrap(),
            Restore::NotArchived,
            "and a second restore finds nothing"
        );
    }

    #[test]
    fn restoring_over_existing_picks_moves_nothing() {
        // Two documents by one person on one radargram are not merged, and
        // neither the picks drawn since nor the archived ones may be lost.
        let (_dir, project) = project();
        let store = project.documents();
        let id = radargram("line-01");
        write_picks(store, &id, "erik");
        archive_all(store, &id, "2026-09-13T12:00:00Z").unwrap();
        write_picks(store, &id, "erik");
        let live = store.read(&path_of(&id, &user("erik"))).unwrap().unwrap();

        let item = only_archived(store);
        assert_eq!(restore_archived(store, &item).unwrap(), Restore::Occupied);
        assert_eq!(
            store
                .read(&path_of(&id, &user("erik")))
                .unwrap()
                .unwrap()
                .version,
            live.version
        );
        assert_eq!(list_archived(store).unwrap().0, vec![item]);
    }

    #[test]
    fn restoring_one_of_a_removal_leaves_the_others_archived() {
        let (_dir, project) = project();
        let store = project.documents();
        let id = radargram("line-01");
        write_picks(store, &id, "erik");
        write_picks(store, &id, "student");
        archive_all(store, &id, "2026-09-13T12:00:00Z").unwrap();

        let (archived, _) = list_archived(store).unwrap();
        let erik = archived.iter().find(|a| a.user.as_str() == "erik").unwrap();
        restore_archived(store, erik).unwrap();

        let (left, _) = list_archived(store).unwrap();
        assert_eq!(
            left.iter().map(|a| a.user.as_str()).collect::<Vec<_>>(),
            vec!["student"]
        );
    }

    /// A filename that is not a valid `UserId` is skipped and *named*, not
    /// silently dropped and not fatal (#213). Archiving is where this matters
    /// most: the whole point is that no orphan stays behind to reattach to a
    /// different radargram under the same id.
    #[test]
    fn an_unreadable_filename_is_reported_rather_than_hidden() {
        let (_dir, project) = project();
        let store = project.documents();
        let id = radargram("line-01");

        // One valid pick, and one whose filename `UserId` refuses.
        write_picks(store, &id, "valid");
        store
            .write(
                &directory_of(&id).join(format!("NotAUserId{SUFFIX}")),
                r#"{"schema":"gprinterp","key":"line-01","features":[]}"#,
                &Expectation::Any,
            )
            .unwrap();

        let (readable, unreadable) = list_users_checked(store, &id).unwrap();
        assert_eq!(
            readable.iter().map(UserId::as_str).collect::<Vec<_>>(),
            vec!["valid"],
            "the valid pick must not be hidden by the invalid one"
        );
        assert_eq!(unreadable, vec!["NotAUserId".to_string()]);

        let report = archive_all(store, &id, "2026-09-20T12:00:00Z").unwrap();
        assert_eq!(report.moved, 1, "the readable pick is archived");
        assert_eq!(
            report.skipped,
            vec!["NotAUserId".to_string()],
            "the unreadable one is named, not silently left behind"
        );
    }
}
