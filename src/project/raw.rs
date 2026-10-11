//! Raw data, and what each radargram is made from (#382, phase 4).
//!
//! Two things live under `ridal_data/raw/`:
//!
//! - **Files**, content-addressed: `files/<aa>/<blake3>`, each stored once
//!   however many radargrams use it. A recording's `.rd3` and its original
//!   and replacement `.cor`s, and a DEM shared by a whole survey, are all
//!   just files here.
//! - **Sources**, one document per radargram id, `sources/<id>.json`: the
//!   recordings it is made from, in merge order, each a list of files by
//!   name and hash, and optionally a DEM and a CRS. Switching a radargram's
//!   source (a better `.cor`, say) is writing this document.
//! - **DEMs**, one document per DEM, `dems/<id>.json`: a DEM uploaded once
//!   for the project, with a name, that any number of sources link to by
//!   id. A source keeps the linked file's hash beside the id, so recipes
//!   and staleness compare hashes as they do for recordings.
//!
//! Nothing counts references. A file is deleted by [`sweep`], when no
//! source or DEM names it and it is older than [`SWEEP_MIN_AGE`]:
//! recomputed from the documents each time, so it cannot drift and
//! survives a crash halfway through a delete, and an upload whose source
//! is not written yet is not swept from under it. Which radargrams use a
//! DEM is computed from the sources in the same way ([`dem_users`]).
//!
//! Raw files count toward the project's `max_bytes` like everything else
//! under the project root.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::store::{DocumentStore, Expectation, StoreError, Version};
use crate::identity::{DemId, RadargramId};
use crate::recipe::{Bundle, BundleFile, FileRole, HashedFile};

/// Relative to the project's data directory.
pub const RAW_DIR: &str = "raw";
const FILES: &str = "files";
const SOURCES: &str = "sources";
const DEMS: &str = "dems";
const INCOMING: &str = "incoming";

/// How old an unreferenced file must be before [`sweep`] deletes it.
pub const SWEEP_MIN_AGE: Duration = Duration::from_secs(60 * 60);

/// What one radargram is made from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Source {
    /// The raw recordings, in the order they are merged.
    pub bundles: Vec<Bundle>,
    /// The CRS to project positions into; the project's default otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crs: Option<String>,
    /// The project's DEM to take elevations from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dem: Option<SourceDem>,
}

impl Source {
    /// Every file the source names.
    pub fn hashes(&self) -> impl Iterator<Item = &str> {
        self.bundles
            .iter()
            .flat_map(|b| b.files.iter().map(|f| f.file.blake3.as_str()))
            .chain(self.dem.iter().map(|d| d.file.blake3.as_str()))
    }

    /// The recordings as a client names them, in merge order.
    pub fn recordings(&self) -> Vec<Vec<NamedFile>> {
        self.bundles
            .iter()
            .map(|bundle| {
                bundle
                    .files
                    .iter()
                    .map(|f| NamedFile {
                        name: f.file.name.clone(),
                        blake3: f.file.blake3.clone(),
                    })
                    .collect()
            })
            .collect()
    }
}

/// A source's link to one of the project's [`Dem`]s: which, and its file
/// when the link was made. A DEM's file cannot change while anything links
/// to it, so the two agree.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceDem {
    pub id: DemId,
    #[serde(flatten)]
    pub file: HashedFile,
}

/// A DEM uploaded once for the project.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Dem {
    pub id: DemId,
    pub name: String,
    pub file: HashedFile,
    /// Who uploaded the file.
    pub by: String,
    /// When the file was uploaded, RFC 3339.
    pub at: String,
}

/// A blake3 digest as 64 lowercase hex characters, or `None`. Every path
/// below is built from one, so this is what keeps them inside the store.
pub fn valid_hash(text: &str) -> Option<&str> {
    (text.len() == 64
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
    .then_some(text)
}

fn raw_dir(data_dir: &Path) -> PathBuf {
    data_dir.join(RAW_DIR)
}

/// Where a stored file is, whether or not it exists.
pub fn file_path(data_dir: &Path, hash: &str) -> Option<PathBuf> {
    let hash = valid_hash(hash)?;
    Some(raw_dir(data_dir).join(FILES).join(&hash[..2]).join(hash))
}

/// Whether the store has the file.
#[cfg(test)]
pub fn contains(data_dir: &Path, hash: &str) -> bool {
    file_path(data_dir, hash).is_some_and(|path| path.is_file())
}

/// A fresh path for an upload to be written to before [`install`]: inside
/// the store, so installing it is a rename on one filesystem.
pub fn incoming_path(data_dir: &Path) -> Result<PathBuf, String> {
    let dir = raw_dir(data_dir).join(INCOMING);
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    Ok(dir.join(format!(
        "{}-{}.part",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
    )))
}

/// Move a written file into the store under its hash, and say which.
///
/// Already stored, the new copy is dropped: the content is the name, so the
/// two are the same file.
pub fn install(data_dir: &Path, written: &Path, name: &str) -> Result<HashedFile, String> {
    let hashed = crate::recipe::hash_file(written)?;
    let destination = file_path(data_dir, &hashed.blake3).expect("blake3 is always valid hex");
    if destination.is_file() {
        let _ = std::fs::remove_file(written);
        // Uploaded again, so young again: otherwise a file that sat
        // unreferenced for an hour could be swept between this upload and
        // the source that names it.
        std::fs::File::options()
            .write(true)
            .open(&destination)
            .and_then(|file| file.set_modified(std::time::SystemTime::now()))
            .map_err(|e| format!("could not refresh {name}: {e}"))?;
    } else {
        let parent = destination.parent().expect("has a parent");
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("could not create {}: {e}", parent.display()))?;
        std::fs::rename(written, &destination)
            .map_err(|e| format!("could not store {name}: {e}"))?;
    }
    Ok(HashedFile {
        name: name.to_string(),
        blake3: hashed.blake3,
    })
}

fn source_document(id: &RadargramId) -> PathBuf {
    Path::new(RAW_DIR)
        .join(SOURCES)
        .join(format!("{}.json", id.as_str()))
}

pub fn read_source(
    store: &DocumentStore,
    id: &RadargramId,
) -> Result<Option<(Source, Version)>, String> {
    let Some(document) = store
        .read(&source_document(id))
        .map_err(|e| e.to_string())?
    else {
        return Ok(None);
    };
    let source = serde_json::from_str(&document.text)
        .map_err(|e| format!("the source of '{id}' cannot be read: {e}"))?;
    Ok(Some((source, document.version)))
}

pub fn write_source(
    store: &DocumentStore,
    id: &RadargramId,
    source: &Source,
    expected: &Expectation,
) -> Result<Version, StoreError> {
    let text = serde_json::to_string_pretty(source).expect("a source is plain data");
    store.write(&source_document(id), &text, expected)
}

/// Remove a radargram's source. `false` when it had none.
pub fn remove_source(
    store: &DocumentStore,
    id: &RadargramId,
    expected: &Expectation,
) -> Result<bool, StoreError> {
    store.remove(&source_document(id), expected)
}

/// The radargram ids that have a source.
pub fn source_ids(store: &DocumentStore) -> Result<Vec<String>, String> {
    store
        .list_stems(&Path::new(RAW_DIR).join(SOURCES), ".json")
        .map_err(|e| e.to_string())
}

fn dem_document(id: &DemId) -> PathBuf {
    Path::new(RAW_DIR)
        .join(DEMS)
        .join(format!("{}.json", id.as_str()))
}

pub fn read_dem(store: &DocumentStore, id: &DemId) -> Result<Option<(Dem, Version)>, String> {
    let Some(document) = store.read(&dem_document(id)).map_err(|e| e.to_string())? else {
        return Ok(None);
    };
    let dem = serde_json::from_str(&document.text)
        .map_err(|e| format!("the DEM '{id}' cannot be read: {e}"))?;
    Ok(Some((dem, document.version)))
}

/// Every DEM, by id.
pub fn list_dems(store: &DocumentStore) -> Result<Vec<Dem>, String> {
    let mut dems = Vec::new();
    let mut stems = store
        .list_stems(&Path::new(RAW_DIR).join(DEMS), ".json")
        .map_err(|e| e.to_string())?;
    stems.sort();
    for stem in stems {
        let id = DemId::new(&stem)
            .map_err(|e| format!("a DEM is named '{stem}', which is not an id: {e}"))?;
        if let Some((dem, _)) = read_dem(store, &id)? {
            dems.push(dem);
        }
    }
    Ok(dems)
}

pub fn write_dem(
    store: &DocumentStore,
    dem: &Dem,
    expected: &Expectation,
) -> Result<Version, StoreError> {
    let text = serde_json::to_string_pretty(dem).expect("a DEM is plain data");
    store.write(&dem_document(&dem.id), &text, expected)
}

/// Remove a DEM's document. `false` when there was none.
pub fn remove_dem(
    store: &DocumentStore,
    id: &DemId,
    expected: &Expectation,
) -> Result<bool, StoreError> {
    store.remove(&dem_document(id), expected)
}

/// Every source, by radargram id.
fn sources(store: &DocumentStore) -> Result<Vec<(RadargramId, Source)>, String> {
    let mut out = Vec::new();
    for stem in source_ids(store)? {
        let id = RadargramId::new(&stem)
            .map_err(|e| format!("a source is named '{stem}', which is not an id: {e}"))?;
        if let Some((source, _)) = read_source(store, &id)? {
            out.push((id, source));
        }
    }
    Ok(out)
}

/// The radargrams linked to each DEM, sorted. A DEM nothing links to is
/// absent.
pub fn dem_users(store: &DocumentStore) -> Result<BTreeMap<DemId, Vec<RadargramId>>, String> {
    let mut users: BTreeMap<DemId, Vec<RadargramId>> = BTreeMap::new();
    for (id, source) in sources(store)? {
        if let Some(dem) = source.dem {
            users.entry(dem.id).or_default().push(id);
        }
    }
    for radargrams in users.values_mut() {
        radargrams.sort();
    }
    Ok(users)
}

/// What [`sweep`] did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Swept {
    pub files: usize,
    pub bytes: u64,
}

/// Delete every stored file, and every abandoned upload, that no source
/// or DEM names and that is older than `min_age`. `keep` names files that
/// are in use although no document names them yet, such as those a
/// running job reads.
///
/// A source or DEM that cannot be read stops the sweep rather than
/// counting as naming nothing, which would delete every file it needs.
pub fn sweep(
    store: &DocumentStore,
    data_dir: &Path,
    keep: &HashSet<String>,
    min_age: Duration,
) -> Result<Swept, String> {
    let mut named: HashSet<String> = keep.clone();
    for (_, source) in sources(store)? {
        named.extend(source.hashes().map(str::to_string));
    }
    for dem in list_dems(store)? {
        named.insert(dem.file.blake3);
    }
    let old_enough = |path: &Path| {
        path.metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|at| at.elapsed().ok())
            .is_some_and(|age| age >= min_age)
    };
    let mut swept = Swept::default();
    let mut remove = |path: &Path| {
        let bytes = path.metadata().map_or(0, |m| m.len());
        if std::fs::remove_file(path).is_ok() {
            swept.files += 1;
            swept.bytes += bytes;
        }
    };
    let files = raw_dir(data_dir).join(FILES);
    for shard in std::fs::read_dir(&files).into_iter().flatten().flatten() {
        for entry in std::fs::read_dir(shard.path())
            .into_iter()
            .flatten()
            .flatten()
        {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if !named.contains(&name) && old_enough(&path) {
                remove(&path);
            }
        }
        let _ = std::fs::remove_dir(shard.path());
    }
    let incoming = raw_dir(data_dir).join(INCOMING);
    for entry in std::fs::read_dir(&incoming).into_iter().flatten().flatten() {
        if old_enough(&entry.path()) {
            remove(&entry.path());
        }
    }
    Ok(swept)
}

/// A file the client names for a recording, before Ridal knows its role.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NamedFile {
    /// The file's name, which says what it is: `DAT_0001_A1.rad`.
    pub name: String,
    /// The blake3 the upload answered with.
    pub blake3: String,
}

/// Lay a source's files out under `dir` with their own names, one
/// directory per recording so that two recordings may share a name, and
/// return each recording's header, which is what a reader is given.
///
/// Links rather than copies where the filesystem allows, so a large
/// recording costs nothing to lay out.
pub fn materialize(
    data_dir: &Path,
    bundles: &[Vec<NamedFile>],
    dir: &Path,
) -> Result<Vec<crate::formats::ResolvedInput>, String> {
    let mut resolved = Vec::new();
    for (index, files) in bundles.iter().enumerate() {
        let recording = dir.join(index.to_string());
        std::fs::create_dir_all(&recording)
            .map_err(|e| format!("could not create {}: {e}", recording.display()))?;
        let mut first = None;
        for file in files {
            let name = plain_name(&file.name)?;
            let stored = file_path(data_dir, &file.blake3)
                .filter(|path| path.is_file())
                .ok_or_else(|| {
                    format!(
                        "{} ({}) has not been uploaded to this project",
                        file.name, file.blake3
                    )
                })?;
            let laid = recording.join(name);
            link(&stored, &laid)?;
            first.get_or_insert(laid);
        }
        let first = first.ok_or_else(|| format!("recording {} has no files", index + 1))?;
        resolved.push(crate::formats::resolve_input(&first)?);
    }
    Ok(resolved)
}

/// Lay one stored file out in `dir` under its own name, as [`materialize`]
/// does a recording's, and return where. For a DEM, whose name is then
/// what the processed file records rather than its hash.
pub fn materialize_file(data_dir: &Path, file: &HashedFile, dir: &Path) -> Result<PathBuf, String> {
    let name = plain_name(&file.name)?;
    let stored = file_path(data_dir, &file.blake3)
        .filter(|path| path.is_file())
        .ok_or_else(|| {
            format!(
                "{} ({}) has not been uploaded to this project",
                file.name, file.blake3
            )
        })?;
    std::fs::create_dir_all(dir).map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    let laid = dir.join(name);
    link(&stored, &laid)?;
    Ok(laid)
}

/// A file name with nothing in it that could leave its directory.
pub fn plain_name(name: &str) -> Result<&str, String> {
    let ok = !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains(['/', '\\', '\0'])
        && Path::new(name).file_name().is_some_and(|n| n == name);
    ok.then_some(name)
        .ok_or_else(|| format!("'{name}' is not a plain file name"))
}

fn link(stored: &Path, laid: &Path) -> Result<(), String> {
    #[cfg(unix)]
    if std::os::unix::fs::symlink(stored, laid).is_ok() {
        return Ok(());
    }
    if std::fs::hard_link(stored, laid).is_ok() {
        return Ok(());
    }
    std::fs::copy(stored, laid)
        .map(|_| ())
        .map_err(|e| format!("could not lay out {}: {e}", laid.display()))
}

/// The recordings of `bundles` as a [`Source`] records them: each file with
/// the role its reader gives it, found from the names by laying them out
/// under `dir`. A file the reader does not use is refused, since a source
/// should say exactly what a radargram is made from.
pub fn bundles_of(
    data_dir: &Path,
    bundles: &[Vec<NamedFile>],
    dir: &Path,
) -> Result<Vec<Bundle>, String> {
    let resolved = materialize(data_dir, bundles, dir)?;
    resolved
        .iter()
        .zip(bundles)
        .map(|(resolved, files)| {
            let role_of = |name: &str| {
                let is = |path: &Path| path.file_name().is_some_and(|n| n == name);
                if is(&resolved.header) {
                    Some(FileRole::Header)
                } else if is(&resolved.data) {
                    Some(FileRole::Data)
                } else if is(&resolved.coordinates) {
                    Some(FileRole::Coordinates)
                } else {
                    None
                }
            };
            let mut out = Vec::new();
            for file in files {
                let role = role_of(&file.name).ok_or_else(|| {
                    format!(
                        "{} is not part of the {} recording {}",
                        file.name,
                        crate::formats::format_info(resolved.kind).name,
                        resolved
                            .header
                            .file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_default()
                    )
                })?;
                out.push(BundleFile {
                    role,
                    file: HashedFile {
                        name: file.name.clone(),
                        blake3: file.blake3.clone(),
                    },
                });
            }
            out.sort_by_key(|f| match f.role {
                FileRole::Header => 0,
                FileRole::Data => 1,
                FileRole::Coordinates => 2,
            });
            Ok(Bundle {
                format: crate::formats::format_info(resolved.kind).name.to_string(),
                files: out,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn upload(data_dir: &Path, path: &str) -> NamedFile {
        let incoming = incoming_path(data_dir).unwrap();
        std::fs::copy(path, &incoming).unwrap();
        let name = Path::new(path).file_name().unwrap().to_string_lossy();
        let stored = install(data_dir, &incoming, &name).unwrap();
        NamedFile {
            name: stored.name,
            blake3: stored.blake3,
        }
    }

    fn recording(data_dir: &Path, stem: &str) -> Vec<NamedFile> {
        ["rad", "rd3", "cor"]
            .iter()
            .map(|ext| upload(data_dir, &format!("assets/mala/{stem}.{ext}")))
            .collect()
    }

    const LINE: &str = "dronbreen-20250327-DAT_0066_A1";

    #[test]
    fn a_file_is_stored_once_under_its_hash() {
        let dir = tempfile::tempdir().unwrap();
        let a = upload(dir.path(), "assets/mala/dronbreen-20250327-DAT_0066_A1.cor");
        let b = upload(dir.path(), "assets/mala/dronbreen-20250327-DAT_0066_A1.cor");
        assert_eq!(a, b);
        assert!(contains(dir.path(), &a.blake3));
        assert_eq!(
            a.blake3,
            crate::recipe::hash_file(Path::new("assets/mala/dronbreen-20250327-DAT_0066_A1.cor"))
                .unwrap()
                .blake3
        );
        let incoming = dir.path().join(RAW_DIR).join(INCOMING);
        assert_eq!(std::fs::read_dir(incoming).unwrap().count(), 0);
    }

    #[test]
    fn only_a_hash_names_a_stored_file() {
        let dir = tempfile::tempdir().unwrap();
        assert!(file_path(dir.path(), "../../etc/passwd").is_none());
        assert!(file_path(dir.path(), &"A".repeat(64)).is_none());
        assert!(file_path(dir.path(), &"a".repeat(64)).is_some());
        assert!(plain_name("../x.rad").is_err());
        assert!(plain_name("a/b.rad").is_err());
        assert!(plain_name("..").is_err());
        assert!(plain_name("DAT_0001_A1.rad").is_ok());
    }

    #[test]
    fn a_recording_gets_its_roles_from_its_names() {
        let dir = tempfile::tempdir().unwrap();
        let files = recording(dir.path(), LINE);
        let work = tempfile::tempdir().unwrap();
        let bundles = bundles_of(dir.path(), std::slice::from_ref(&files), work.path()).unwrap();
        assert_eq!(bundles[0].format, "ramac");
        let roles: Vec<FileRole> = bundles[0].files.iter().map(|f| f.role).collect();
        assert_eq!(
            roles,
            [FileRole::Header, FileRole::Data, FileRole::Coordinates]
        );
        // The same as the recipe a run from these files would record.
        let resolved =
            crate::formats::resolve_input(Path::new(&format!("assets/mala/{LINE}.rad"))).unwrap();
        assert_eq!(
            bundles[0],
            crate::recipe::bundle_of(&resolved, None).unwrap()
        );

        // A file the recording does not use is refused.
        let mut extra = files;
        extra.push(upload(dir.path(), "assets/mala/README.md"));
        let refused =
            bundles_of(dir.path(), &[extra], tempfile::tempdir().unwrap().path()).unwrap_err();
        assert!(refused.contains("README.md is not part of"), "{refused}");
    }

    #[test]
    fn a_file_not_uploaded_is_named() {
        let dir = tempfile::tempdir().unwrap();
        let missing = NamedFile {
            name: "DAT_0001_A1.rad".to_string(),
            blake3: "a".repeat(64),
        };
        let refused = materialize(dir.path(), &[vec![missing]], dir.path()).unwrap_err();
        assert!(refused.contains("has not been uploaded"), "{refused}");
    }

    #[test]
    fn a_laid_out_source_reads_as_the_original_files_do() {
        let dir = tempfile::tempdir().unwrap();
        let files = recording(dir.path(), LINE);
        let work = tempfile::tempdir().unwrap();
        let resolved = materialize(dir.path(), &[files], work.path()).unwrap();
        let laid = crate::formats::resolve_input(&resolved[0].header).unwrap();
        assert_eq!(
            crate::recipe::bundle_of(&laid, None).unwrap().files,
            crate::recipe::bundle_of(
                &crate::formats::resolve_input(Path::new(&format!("assets/mala/{LINE}.rad")))
                    .unwrap(),
                None
            )
            .unwrap()
            .files
        );
    }

    #[test]
    fn a_sweep_keeps_what_a_source_or_a_job_needs_and_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        let store = DocumentStore::new(dir.path().to_path_buf());
        let files = recording(dir.path(), LINE);
        let work = tempfile::tempdir().unwrap();
        let bundles = bundles_of(dir.path(), std::slice::from_ref(&files), work.path()).unwrap();
        let id = RadargramId::new("line-01").unwrap();
        let source = Source {
            bundles,
            crs: None,
            dem: None,
        };
        write_source(&store, &id, &source, &Expectation::Absent).unwrap();
        let loose = upload(dir.path(), "assets/mala/README.md");
        let kept = upload(dir.path(), "assets/mala/dronbreen-20220329-DAT_0237_A1.cor");
        // Abandoned halfway.
        std::fs::write(incoming_path(dir.path()).unwrap(), b"partial").unwrap();

        // Too young to sweep: an upload whose source is not written yet.
        let keep: HashSet<String> = [kept.blake3.clone()].into();
        let swept = sweep(&store, dir.path(), &keep, SWEEP_MIN_AGE).unwrap();
        assert_eq!(swept, Swept::default());

        let swept = sweep(&store, dir.path(), &keep, Duration::ZERO).unwrap();
        assert_eq!(swept.files, 2, "the loose file and the partial upload");
        assert!(!contains(dir.path(), &loose.blake3));
        assert!(contains(dir.path(), &kept.blake3));
        assert!(files.iter().all(|f| contains(dir.path(), &f.blake3)));

        // With the source gone, so are its files.
        assert!(remove_source(&store, &id, &Expectation::Any).unwrap());
        sweep(&store, dir.path(), &HashSet::new(), Duration::ZERO).unwrap();
        assert!(files.iter().all(|f| !contains(dir.path(), &f.blake3)));
        assert!(source_ids(&store).unwrap().is_empty());
    }

    #[test]
    fn a_dem_keeps_its_file_and_knows_who_links_to_it() {
        let dir = tempfile::tempdir().unwrap();
        let store = DocumentStore::new(dir.path().to_path_buf());
        let file = upload(dir.path(), "assets/test_dem_dtm20_mettebreen.tif");
        let id = DemId::new("mettebreen").unwrap();
        let dem = Dem {
            id: id.clone(),
            name: "Mettebreen DTM20".to_string(),
            file: HashedFile {
                name: file.name.clone(),
                blake3: file.blake3.clone(),
            },
            by: "erik".to_string(),
            at: "2026-10-11T00:00:00Z".to_string(),
        };
        write_dem(&store, &dem, &Expectation::Absent).unwrap();
        assert_eq!(list_dems(&store).unwrap(), vec![dem.clone()]);

        // Linked by nothing, it is still kept.
        sweep(&store, dir.path(), &HashSet::new(), Duration::ZERO).unwrap();
        assert!(contains(dir.path(), &file.blake3));
        assert!(dem_users(&store).unwrap().is_empty());

        let files = recording(dir.path(), LINE);
        let work = tempfile::tempdir().unwrap();
        let bundles = bundles_of(dir.path(), std::slice::from_ref(&files), work.path()).unwrap();
        let line = RadargramId::new("line-01").unwrap();
        let source = Source {
            bundles,
            crs: None,
            dem: Some(SourceDem {
                id: id.clone(),
                file: dem.file.clone(),
            }),
        };
        write_source(&store, &line, &source, &Expectation::Absent).unwrap();
        assert_eq!(dem_users(&store).unwrap()[&id], vec![line.clone()]);
        // The source's own copy of the hash keeps the file too.
        assert!(remove_dem(&store, &id, &Expectation::Any).unwrap());
        sweep(&store, dir.path(), &HashSet::new(), Duration::ZERO).unwrap();
        assert!(contains(dir.path(), &file.blake3));
        remove_source(&store, &line, &Expectation::Any).unwrap();
        sweep(&store, dir.path(), &HashSet::new(), Duration::ZERO).unwrap();
        assert!(!contains(dir.path(), &file.blake3));
    }

    #[test]
    fn a_source_that_cannot_be_read_stops_the_sweep() {
        let dir = tempfile::tempdir().unwrap();
        let store = DocumentStore::new(dir.path().to_path_buf());
        let kept = upload(dir.path(), "assets/mala/README.md");
        let path = dir.path().join(RAW_DIR).join(SOURCES);
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join("line-01.json"), "{").unwrap();
        assert!(sweep(&store, dir.path(), &HashSet::new(), Duration::ZERO).is_err());
        assert!(contains(dir.path(), &kept.blake3));
    }
}
