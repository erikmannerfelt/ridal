//! Recursive multi-radargram catalog discovery (#122).
//!
//! Discovers processed Ridal radargrams under a single file or a directory
//! root, resolving duplicate persistent IDs deterministically and reporting
//! discovery problems as warnings rather than aborting the whole catalog.

// Consumed by the HTTP application and index page (M6/M7); until then the
// only callers are this module's own tests.
#![allow(dead_code)]

use std::path::Path;

pub use crate::identity::RevisionId;
use crate::identity::{DisplayName, GroupId, GroupName, RadargramId};
use crate::io::{self, RidalNetcdfKind};
use crate::project::overrides::CatalogOverrides;

/// One discovered radargram, selected as the representative for its
/// `radargram_id` if duplicates were found.
#[derive(Debug, Clone, PartialEq)]
pub struct CatalogEntry {
    pub radargram_id: RadargramId,
    pub revision_id: RevisionId,
    pub display_name: Option<DisplayName>,
    pub group_name: Option<GroupName>,
    pub group_id: Option<GroupId>,
    pub processing_datetime: String,
    pub shape: (usize, usize),
    /// Catalog-relative path, `/`-normalized regardless of platform. UI
    /// disambiguation only -- not part of the persistent identity or
    /// revision fingerprint (#122).
    pub relative_path: String,
    /// Which of the catalog's roots this was found under, as an index into
    /// the list discovery was given.
    ///
    /// An index rather than a path because the path is the server's
    /// business and #122 keeps those internal; `relative_path` is what the
    /// UI shows and it only means something paired with its root.
    pub root: usize,
    /// Set by a project override (#145). Curation, not access control: an
    /// unlisted radargram is left out of listings and still reachable by
    /// anyone who knows its id.
    pub unlisted: bool,
    /// Set by a project override (#168): the floor of the corrected
    /// view's raster, and the cap on a trace's surface elevation. See
    /// [`crate::project::overrides::RadargramOverride`] for what each one
    /// does -- they are not two ends of one range. `None` on either side
    /// means no bound. Files never carry a counterpart for these -- they
    /// have no `from_file` entry, unlike
    /// `display_name`/`group_name`/`group_id` -- since sane bounds for a
    /// survey are a project decision, not something a processed file
    /// states about itself.
    pub elevation_min: Option<f64>,
    pub elevation_max: Option<f64>,
    /// What the file said, before the project's overrides (#145).
    ///
    /// Kept beside the resolved values so the Edit properties dialog can
    /// show what each field *would* be without its override, and offer to
    /// revert to it. Without that, an override silently shadows a later
    /// reprocessing that set the attribute properly, and nothing on screen
    /// explains why the new name did not take.
    pub from_file: FileMetadata,
}

/// The catalog metadata a radargram's own file carries.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FileMetadata {
    pub display_name: Option<DisplayName>,
    pub group_name: Option<GroupName>,
    pub group_id: Option<GroupId>,
}

impl CatalogEntry {
    /// `ridal_display_name` if present and non-empty, else `radargram_id`
    /// (#116's GUI labeling rule).
    pub fn effective_label(&self) -> String {
        match &self.display_name {
            Some(name) => name.to_string(),
            None => self.radargram_id.to_string(),
        }
    }

    /// The bounds the topographically corrected view (#168) should render
    /// this radargram within.
    pub fn elevation_range(&self) -> crate::render::topo::ElevationRange {
        crate::render::topo::ElevationRange {
            min: self.elevation_min,
            max: self.elevation_max,
        }
    }
}

/// A non-fatal problem encountered during discovery: an unreadable
/// candidate, an inspection error, or a duplicate radargram ID collision.
#[derive(Debug, Clone, PartialEq)]
pub struct CatalogWarning {
    pub message: String,
    /// Radargrams the message names, so a listing can drop a warning it is
    /// not allowed to show.
    ///
    /// These messages quote ids and paths. An unlisted radargram is meant
    /// to be absent from a `picker`'s listing, and a warning saying "there
    /// are two of `dronbreen-0237`, here and here" would put it back --
    /// which is the same leak as drawing its track on the group map, one
    /// paragraph further down the page.
    ///
    /// Empty means the message names no radargram in the catalog, which is
    /// the unreadable-candidate case: a file that failed inspection is not
    /// an entry, so it cannot be one somebody unlisted.
    pub about: Vec<RadargramId>,
    /// Shown only to `operator` and above.
    ///
    /// For messages about radargrams the catalog deliberately does not
    /// serve. `about` cannot express that: it drops a warning when the
    /// caller cannot see the radargrams it names, and an ignored radargram
    /// is in *nobody's* listing -- so a warning naming one would be
    /// invisible to the very person who made the decision. The role gate is
    /// the separate question of who is entitled to hear about it at all.
    pub operator_only: bool,
}

impl CatalogWarning {
    /// Whether this warning can be shown to someone who sees only
    /// `visible`.
    pub fn is_visible_to<'a>(&self, mut visible: impl Iterator<Item = &'a RadargramId>) -> bool {
        self.about
            .iter()
            .all(|named| visible.any(|shown| shown == named))
    }
}

/// The result of discovering radargrams under one root (a file or a
/// directory).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Catalog {
    /// Everything the project serves, with the ignored left out.
    pub entries: Vec<CatalogEntry>,
    /// Everything discovery found, before any override was applied.
    ///
    /// Kept so re-resolution starts where discovery did. Re-resolving from
    /// `entries` could only ever *narrow* the catalog: an ignored radargram
    /// is not in them, so lifting the ignore could never bring it back, and
    /// a project that un-ignores something would have to be restarted to
    /// see it.
    ///
    /// It is also what makes re-resolution honest about the rest. These
    /// entries carry the files' own values, so the resolved ones are built
    /// from the same starting point every time rather than being un-applied
    /// and re-applied.
    unresolved: Vec<CatalogEntry>,
    /// Everything worth telling the operator: problems with the files
    /// themselves, and groups whose members disagree about the name.
    pub warnings: Vec<CatalogWarning>,
    /// Just the first kind, kept so [`Catalog::reresolved`] can rebuild the
    /// combined list. Group-name disagreements depend on the overrides and
    /// are recomputed; unreadable files and duplicate ids do not and are
    /// not.
    file_warnings: Vec<CatalogWarning>,
    /// One representative display name per group id, for the index page
    /// (a group has one heading, even though every entry carries its own
    /// `group_name` for provenance). When entries sharing a `group_id`
    /// disagree on the name, resolved exactly like a duplicate
    /// `radargram_id` (#122): most recent `processing_datetime` wins, ties
    /// broken by path order, with a `CatalogWarning` either way.
    pub group_names: std::collections::BTreeMap<GroupId, GroupName>,
    /// Ignore decisions with nothing to act on (#147).
    vestigial_ignores: Vec<VestigialIgnore>,
}

/// One place radargrams are found, and what Ridal may do there.
///
/// The project's own radargram directory is the writable upper layer;
/// anything `[radargrams] roots` points at outside it is a read-only lower
/// one (#147). Ridal never writes below the project — not "unless an admin
/// unlocks it", never — so an external archive can be served without any
/// question of what a wrong click there would do.
#[derive(Debug, Clone, PartialEq)]
pub struct CatalogRoot {
    /// Canonicalized once, by the caller, and the single point every later
    /// filesystem operation on it flows from. See [`Catalog::discover`].
    pub path: std::path::PathBuf,
    /// Whether `path` names one `.nc` file rather than a directory.
    pub is_file: bool,
    /// Whether Ridal may write here.
    pub writable: bool,
    /// Directories under `path` whose own name is structure rather than
    /// grouping: the project's declared `[radargrams] roots`.
    ///
    /// The group fallback reads the parent directory as a group name,
    /// which is right for an archive laid out as `Drønbreen/2022/` and
    /// wrong for the project's own `radargrams/`. Serving a project root
    /// put every file in it one level down, so a radargram with no group
    /// metadata of any kind came out in a group called "radargrams" — a
    /// name nobody chose, from a directory that only exists because a
    /// project has to keep its files somewhere.
    ///
    /// Group hints are taken relative to the deepest of these that
    /// contains the file, so `radargrams/Drønbreen/2022/line.nc` still
    /// groups by "Drønbreen/2022" and `radargrams/line.nc` is ungrouped.
    pub group_bases: Vec<std::path::PathBuf>,
}

impl CatalogRoot {
    /// A single root, for the callers that have one: a bare directory, a
    /// single file, or a test.
    pub fn single(path: impl Into<std::path::PathBuf>) -> Self {
        let path = path.into();
        let is_file = path.is_file();
        Self {
            path,
            is_file,
            writable: true,
            group_bases: Vec::new(),
        }
    }
}

/// Directory names that recursive discovery does not descend into,
/// regardless of the platform's directory-symlink conventions (#122: "do
/// not follow directory symlinks by default; ignore hidden and cache
/// directories where appropriate").
fn is_excluded_dir_name(name: &str) -> bool {
    name == ".git" || name.starts_with('.')
}

/// A `(path, group_hint)` pair for one discovered `.nc` candidate, where
/// `group_hint` is the catalog-relative parent directory (the group
/// fallback source when `ridal_group` is absent from the file itself).
struct Candidate {
    path: std::path::PathBuf,
    relative_path: String,
    group_hint: Option<String>,
    /// Index of the root this was found under.
    root: usize,
}

fn discover_candidates(root: &CatalogRoot, index: usize) -> Vec<Candidate> {
    let is_file = root.is_file;
    let group_bases = root.group_bases.clone();
    let root = root.path.as_path();
    if is_file {
        let name = root
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        return vec![Candidate {
            path: root.to_path_buf(),
            relative_path: name,
            group_hint: None,
            root: index,
        }];
    }

    let mut candidates = Vec::new();
    let walker = walkdir::WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| {
            // The root itself (depth 0) is exempt: the caller explicitly
            // chose to scan it, dot-prefixed or not -- e.g. tempfile's
            // tempdir() names its directories ".tmpXXXXXX", which would
            // otherwise prune the entire walk before it starts. Exclusion
            // only applies to descendants, and (for a directory) prunes
            // everything beneath it.
            if entry.depth() == 0 {
                return true;
            }
            if entry.file_type().is_dir() {
                let name = entry.file_name().to_string_lossy();
                !is_excluded_dir_name(&name)
            } else {
                true
            }
        });

    for entry in walker {
        // A read error for one entry (permissions, a broken symlink target,
        // etc.) does not abort discovery of the rest of the tree.
        let Ok(entry) = entry else { continue };
        if !entry.file_type().is_file() {
            continue;
        }
        if entry.path().extension().and_then(|e| e.to_str()) != Some("nc") {
            continue;
        }

        let relative = entry
            .path()
            .strip_prefix(root)
            .unwrap_or(entry.path())
            .to_path_buf();
        // Normalize to '/' regardless of platform, per #122.
        let relative_path = relative
            .components()
            .map(|c| c.as_os_str().to_string_lossy().to_string())
            .collect::<Vec<_>>()
            .join("/");
        // Relative to the deepest declared radargram directory containing
        // this file, where there is one, rather than to the served root.
        // The deepest, because a project may declare both a directory and
        // something inside it, and the innermost is the one whose name is
        // structure.
        let hint_base = group_bases
            .iter()
            .filter(|base| entry.path().starts_with(base))
            .max_by_key(|base| base.components().count());
        let for_hint = match hint_base {
            Some(base) => entry.path().strip_prefix(base).unwrap_or(&relative),
            None => relative.as_path(),
        };
        let group_hint = for_hint
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(|p| {
                p.components()
                    .map(|c| c.as_os_str().to_string_lossy().to_string())
                    .collect::<Vec<_>>()
                    .join("/")
            });

        candidates.push(Candidate {
            path: entry.into_path(),
            relative_path,
            group_hint,
            root: index,
        });
    }

    // Deterministic ordering (#122), independent of filesystem iteration
    // order.
    candidates.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
    candidates
}

impl Catalog {
    /// Discover radargrams under `root`, taking the files at their word.
    pub fn discover(root: &Path) -> Catalog {
        Self::discover_with_overrides(root, &CatalogOverrides::default())
    }

    /// Discover radargrams across several roots, the project's own first
    /// (#147).
    ///
    /// The project is the writable upper layer and external roots the
    /// read-only lower ones, and the upper layer wins: where two roots hold
    /// the same radargram id, the writable one is selected **regardless of
    /// processing datetime**. That is a real change from the single-root
    /// rule, and it is the point — otherwise an external file reprocessed
    /// later would silently override a deliberate in-project decision, and
    /// the project would not be an overlay at all.
    pub fn discover_roots(roots: &[CatalogRoot], overrides: &CatalogOverrides) -> Catalog {
        let candidates: Vec<Candidate> = roots
            .iter()
            .enumerate()
            .flat_map(|(index, root)| discover_candidates(root, index))
            .collect();
        Self::from_candidates(candidates, roots, overrides)
    }

    /// Discover radargrams under `root`, which may be a single processed
    /// `.nc` file or a directory to scan recursively, and apply what the
    /// project says over them (#145).
    ///
    /// The overrides are applied here rather than to a finished catalog
    /// because group-name resolution depends on them: a group the project
    /// has named authoritatively cannot disagree with itself, so it must
    /// not produce the disagreement warning, and a radargram moved into
    /// another group must not bring its old group's name along.
    pub fn discover_with_overrides(root: &Path, overrides: &CatalogOverrides) -> Catalog {
        Self::discover_roots(&[CatalogRoot::single(root)], overrides)
    }

    fn from_candidates(
        candidates: Vec<Candidate>,
        roots: &[CatalogRoot],
        overrides: &CatalogOverrides,
    ) -> Catalog {
        let mut warnings = Vec::new();
        // (is the layer writable, id) -> every (datetime, path) found there.
        let mut by_layer: std::collections::BTreeMap<(bool, RadargramId), Vec<(String, String)>> =
            std::collections::BTreeMap::new();
        let mut by_id: std::collections::BTreeMap<String, (CatalogEntry, String)> =
            std::collections::BTreeMap::new();

        for candidate in candidates {
            let inspection = match io::inspect_ridal_netcdf(&candidate.path) {
                Ok(k) => k,
                Err(e) => {
                    warnings.push(CatalogWarning {
                        message: format!("{}: {e}", candidate.relative_path),
                        about: Vec::new(),
                        operator_only: false,
                    });
                    continue;
                }
            };
            let RidalNetcdfKind::Supported(meta) = inspection else {
                continue; // NotRidal: silently ignored, per #122/#123.
            };

            // The file's own ridal_group_name/ridal_group_id win; absent
            // that, fall back to the catalog-relative parent directory as
            // both the name and (derived) id -- e.g. a file discovered
            // under "Drønbreen/2022/" with no group metadata of its own
            // gets name "Drønbreen", id "dronbreen".
            let (group_name, group_id) = match meta.group_name {
                Some(name) => (Some(name), meta.group_id),
                None => match candidate
                    .group_hint
                    .as_deref()
                    .and_then(GroupName::from_input)
                {
                    Some(name) => {
                        let id = GroupId::from_fallback(name.as_str()).ok();
                        (Some(name), id)
                    }
                    None => (None, None),
                },
            };

            let revision_id =
                RevisionId::fingerprint_v1(&meta.radargram_id, &meta.processing_datetime);
            let display_name = meta.display_name;
            let entry = CatalogEntry {
                radargram_id: meta.radargram_id.clone(),
                revision_id,
                display_name: display_name.clone(),
                group_name: group_name.clone(),
                group_id: group_id.clone(),
                processing_datetime: meta.processing_datetime,
                shape: meta.shape,
                relative_path: candidate.relative_path.clone(),
                unlisted: false,
                elevation_min: None,
                elevation_max: None,
                root: candidate.root,
                from_file: FileMetadata {
                    display_name: display_name.clone(),
                    group_name: group_name.clone(),
                    group_id: group_id.clone(),
                },
            };

            let writable = roots.get(entry.root).is_some_and(|root| root.writable);
            // Every occurrence, per layer, kept apart from the question of
            // which one wins. Reporting from the winner meant that with one
            // project copy and two external ones, both externals compared
            // against the project entry, both took the overlay branch, and
            // the duplicate *between them* -- a real problem with no
            // decision behind it -- went unreported entirely.
            by_layer
                .entry((writable, entry.radargram_id.clone()))
                .or_default()
                .push((
                    entry.processing_datetime.clone(),
                    candidate.relative_path.clone(),
                ));

            let id_key = meta.radargram_id.as_str().to_string();
            let new_wins = match by_id.get(&id_key) {
                None => true,
                Some((existing, existing_path)) => {
                    let existing_writable =
                        roots.get(existing.root).is_some_and(|root| root.writable);
                    if writable != existing_writable {
                        // The overlay beats the datetime (#147). Without
                        // this, an external file reprocessed later would
                        // silently override a deliberate in-project
                        // decision, and the project would not be an overlay
                        // at all. Not a warning: the arrangement working is
                        // not a problem to report.
                        writable
                    } else {
                        // Within one layer, deterministically (#122): most
                        // recent ridal_processing_datetime wins, ties break
                        // by relative path.
                        entry.processing_datetime > existing.processing_datetime
                            || (entry.processing_datetime == existing.processing_datetime
                                && candidate.relative_path < *existing_path)
                    }
                }
            };
            if new_wins {
                by_id.insert(id_key, (entry, candidate.relative_path));
            }
        }

        // One warning per layer that holds an id twice. Exact copies are
        // reported too, even though the selected entry is unambiguous, so
        // the user is nudged toward assigning unique ids.
        for ((_, id), mut found) in by_layer {
            if found.len() < 2 {
                continue;
            }
            found.sort();
            let tied = found.windows(2).any(|w| w[0].0 == w[1].0);
            let paths: Vec<&str> = found.iter().map(|(_, path)| path.as_str()).collect();
            warnings.push(CatalogWarning {
                about: vec![id.clone()],
                operator_only: false,
                message: format!(
                    "Duplicate radargram ID '{id}': {}. Selected the entry with the most \
                     recent processing datetime{}.",
                    paths
                        .iter()
                        .map(|path| format!("'{path}'"))
                        .collect::<Vec<_>>()
                        .join(" and "),
                    if tied {
                        " (datetimes equal; broke the tie by path order)"
                    } else {
                        ""
                    }
                ),
            });
        }

        let mut entries: Vec<CatalogEntry> = by_id.into_values().map(|(e, _)| e).collect();
        entries.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));

        resolve(entries, warnings, overrides)
    }

    /// Re-resolve against a different set of overrides, without touching
    /// the disk.
    ///
    /// What a label edit needs. Rediscovery would re-walk the tree and
    /// re-read every file's attributes to learn nothing new — a hundred
    /// NetCDF headers to rename one card — and it would put a filesystem
    /// walk on the end of an HTTP request, which is a worse shape than the
    /// cost alone suggests.
    ///
    /// [`CatalogEntry::from_file`] is what makes this possible: every field
    /// an override can change keeps the file's own value beside it, so the
    /// starting point discovery resolved from is still here. Both paths go
    /// through the same [`resolve`], so the two cannot drift.
    ///
    /// Deliberately blind to files appearing or disappearing on disk. An
    /// override changes what the catalog *says*, never what is in it;
    /// noticing a new file is #147's job.
    pub fn reresolved(&self, overrides: &CatalogOverrides) -> Catalog {
        resolve(
            self.unresolved.clone(),
            self.file_warnings.clone(),
            overrides,
        )
    }
}

/// An ignore decision whose radargram is no longer anywhere Ridal looks.
///
/// Not an error and not automatically cleared. An external root can be
/// unmounted for a week and come back, and forgetting the decision because
/// the disk was busy would quietly start serving something somebody chose
/// not to serve. But a decision nothing can act on should be *visible*
/// rather than accumulating silently, which is what this is for.
#[derive(Debug, Clone, PartialEq)]
pub struct VestigialIgnore {
    pub radargram_id: RadargramId,
    pub since: Option<String>,
}

impl Catalog {
    /// Ignore decisions about radargrams that are not currently found.
    ///
    /// Needs the raw discovery rather than `entries`, since an ignored
    /// radargram is deliberately absent from those -- so this is computed
    /// during discovery and kept.
    pub fn vestigial_ignores(&self) -> &[VestigialIgnore] {
        &self.vestigial_ignores
    }
}

/// Apply the overrides to freshly-unresolved entries and settle the group
/// names. The one place resolution happens, so discovery and re-resolution
/// cannot disagree.
fn resolve(
    unresolved: Vec<CatalogEntry>,
    file_warnings: Vec<CatalogWarning>,
    overrides: &CatalogOverrides,
) -> Catalog {
    // Dropped here rather than during discovery, so both paths make the
    // decision in the same place and lifting an ignore brings the radargram
    // back without re-reading the disk. An ignored radargram is not an
    // entry that happens to be hidden; it is one the project has said it
    // does not serve.
    let mut ignored_changed = Vec::new();
    let mut entries: Vec<CatalogEntry> = unresolved
        .iter()
        .filter(|entry| {
            let Some(decision) = overrides.ignored.get(&entry.radargram_id) else {
                return true;
            };
            // Still ignored -- the decision is on the id, not the file, so
            // replacing the file does not un-ignore it. But it is worth
            // saying: something deliberately not shown becoming a different
            // thing deliberately not shown is exactly the case where
            // whoever made the decision would want to look again.
            //
            // Without this the recorded revision was inert: stored on the
            // way in and never compared to anything.
            if let Some(was) = &decision.revision_id {
                if was != entry.revision_id.as_str() {
                    ignored_changed.push((entry.radargram_id.clone(), was.clone()));
                }
            }
            false
        })
        .cloned()
        .collect();

    for entry in &mut entries {
        apply_override(entry, overrides);
    }

    let (group_names, group_warnings) = resolve_group_names(&entries, overrides);

    // Every member takes the group's resolved name, not just the ones that
    // arrived without one.
    //
    // Filling only the gaps left two ways for a card to disagree with the
    // heading above it: a project that renames a group whose members carry
    // the old name in their files, and a group whose members disagree among
    // themselves, where the heading shows the winner and each card shows
    // its own. `group_name` is the name of the group this entry is in, and
    // there is one of those; what the file called it is kept in
    // `from_file` for anyone who wants the provenance.
    for entry in &mut entries {
        if let Some(id) = &entry.group_id {
            entry.group_name = group_names.get(id).cloned();
        }
    }

    // A warning about a radargram the project does not serve is a report
    // on a decision rather than a problem: two copies of an ignored id are
    // two copies of something nobody sees.
    let mut warnings: Vec<CatalogWarning> = file_warnings
        .iter()
        .filter(|warning| {
            !warning
                .about
                .iter()
                .any(|id| overrides.ignored.contains_key(id))
        })
        .cloned()
        .collect();
    warnings.extend(group_warnings);

    for (id, was) in ignored_changed {
        warnings.push(CatalogWarning {
            // Not in `about`: the radargram is in nobody's entries, so
            // naming it there would hide this from everyone.
            about: Vec::new(),
            operator_only: true,
            message: format!(
                "'{id}' is ignored, and the file behind it has changed since that \
                 decision was made (it was revision {was}). It is still not being \
                 served; restore it if the new content should be."
            ),
        });
    }

    let vestigial_ignores = overrides
        .ignored
        .iter()
        .filter(|(id, _)| !unresolved.iter().any(|e| &e.radargram_id == *id))
        .map(|(id, ignored)| VestigialIgnore {
            radargram_id: id.clone(),
            since: ignored.since.clone(),
        })
        .collect();

    Catalog {
        entries,
        unresolved,
        warnings,
        file_warnings,
        group_names,
        vestigial_ignores,
    }
}

/// Apply what the project says about one radargram, per field.
///
/// Absent means inherit, so setting only a display name leaves the grouping
/// as the file has it.
fn apply_override(entry: &mut CatalogEntry, overrides: &CatalogOverrides) {
    let Some(over) = overrides.radargrams.get(&entry.radargram_id) else {
        return;
    };
    if let Some(name) = &over.display_name {
        entry.display_name = Some(name.clone());
    }
    if let Some(membership) = &over.group {
        let new_id = membership.id();
        if entry.group_id.as_ref() != new_id {
            // The name in the file is the name of the group this radargram
            // was *processed* into, and it has just been moved out of it.
            entry.group_name = None;
        }
        entry.group_id = new_id.cloned();
    }
    entry.unlisted = over.unlisted;
    entry.elevation_min = over.elevation_min;
    entry.elevation_max = over.elevation_max;
}

/// One representative name per group id (see [`Catalog::group_names`]):
/// resolved with the same rule as a duplicate `radargram_id`, applied one
/// level up, except where the project has settled the question itself.
fn resolve_group_names(
    entries: &[CatalogEntry],
    overrides: &CatalogOverrides,
) -> (
    std::collections::BTreeMap<GroupId, GroupName>,
    Vec<CatalogWarning>,
) {
    let mut warnings = Vec::new();
    // (name, processing datetime, path, radargram) -- the last so a
    // disagreement warning can say which two entries it is about, and be
    // withheld from someone who may not see one of them.
    let mut group_name_state: std::collections::BTreeMap<
        GroupId,
        (GroupName, String, String, RadargramId),
    > = std::collections::BTreeMap::new();
    for entry in entries {
        let (Some(id), Some(name)) = (&entry.group_id, &entry.group_name) else {
            continue;
        };
        // A group the project has named cannot disagree with itself. The
        // whole point of the group override is that the name lives in one
        // place, so neither the tie-break nor its warning applies -- and
        // reporting a disagreement an operator has already settled would
        // send them to fix something that is already fixed.
        if overrides
            .groups
            .get(id)
            .and_then(|group| group.name.as_ref())
            .is_some()
        {
            continue;
        }
        match group_name_state.get(id) {
            None => {
                group_name_state.insert(
                    id.clone(),
                    (
                        name.clone(),
                        entry.processing_datetime.clone(),
                        entry.relative_path.clone(),
                        entry.radargram_id.clone(),
                    ),
                );
            }
            Some((existing_name, existing_dt, existing_path, existing_id)) => {
                if existing_name != name {
                    let new_is_newer = entry.processing_datetime > *existing_dt;
                    let tie_new_wins = entry.processing_datetime == *existing_dt
                        && entry.relative_path < *existing_path;

                    warnings.push(CatalogWarning {
                        about: vec![entry.radargram_id.clone(), existing_id.clone()],
                        operator_only: false,
                        message: format!(
                            "Group '{id}' has disagreeing names: '{existing_name}' \
                             ('{existing_path}') and '{name}' ('{}'). Using the name from \
                             the entry with the most recent processing datetime{}.",
                            entry.relative_path,
                            if entry.processing_datetime == *existing_dt {
                                " (datetimes equal; broke the tie by path order)"
                            } else {
                                ""
                            }
                        ),
                    });

                    if new_is_newer || tie_new_wins {
                        group_name_state.insert(
                            id.clone(),
                            (
                                name.clone(),
                                entry.processing_datetime.clone(),
                                entry.relative_path.clone(),
                                entry.radargram_id.clone(),
                            ),
                        );
                    }
                }
            }
        }
    }

    let mut group_names: std::collections::BTreeMap<GroupId, GroupName> = group_name_state
        .into_iter()
        .map(|(id, (name, _, _, _))| (id, name))
        .collect();

    // The project's name wins, for groups that have members. A stale
    // override naming a group nothing is in must not conjure an empty
    // heading onto the index.
    let populated: std::collections::BTreeSet<&GroupId> =
        entries.iter().filter_map(|e| e.group_id.as_ref()).collect();
    for (id, group) in &overrides.groups {
        if let (Some(name), true) = (&group.name, populated.contains(id)) {
            group_names.insert(id.clone(), name.clone());
        }
    }

    (group_names, warnings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gpr::{self, RunParams};

    fn process_to(
        input: &str,
        output: &std::path::Path,
        radargram_id: Option<&str>,
        group: Option<&str>,
    ) {
        let params = RunParams {
            filepaths: vec![std::path::PathBuf::from(input)],
            output_path: Some(output.to_path_buf()),
            dem_path: None,
            cor_path: None,
            medium_velocity: 0.168,
            crs: None,
            quiet: true,
            track_path: None,
            steps: vec!["subset(0 -1 0 50)".to_string()],
            no_export: false,
            render_path: None,
            render_profile: None,
            render_width: None,
            override_antenna_mhz: None,
            override_antenna_separation: None,
            user_metadata: Default::default(),
            radargram_id: radargram_id.map(str::to_string),
            display_name: None,
            group: group.map(str::to_string),
            group_id: None,
        };
        gpr::run(params).unwrap();
    }

    /// Like `process_to`, but with an explicit group id override, for
    /// exercising that precedence tier specifically.
    fn process_to_with_group_id(
        input: &str,
        output: &std::path::Path,
        radargram_id: Option<&str>,
        group: &str,
        group_id: &str,
    ) {
        let params = RunParams {
            filepaths: vec![std::path::PathBuf::from(input)],
            output_path: Some(output.to_path_buf()),
            dem_path: None,
            cor_path: None,
            medium_velocity: 0.168,
            crs: None,
            quiet: true,
            track_path: None,
            steps: vec!["subset(0 -1 0 50)".to_string()],
            no_export: false,
            render_path: None,
            render_profile: None,
            render_width: None,
            override_antenna_mhz: None,
            override_antenna_separation: None,
            user_metadata: Default::default(),
            radargram_id: radargram_id.map(str::to_string),
            display_name: None,
            group: Some(group.to_string()),
            group_id: Some(group_id.to_string()),
        };
        gpr::run(params).unwrap();
    }

    const ASSET_2022: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/assets/mala/dronbreen-20220329-DAT_0237_A1.rad"
    );
    const ASSET_2025: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/assets/mala/dronbreen-20250327-DAT_0066_A1.rad"
    );

    #[test]
    #[test_retry::retry]
    #[serial_test::serial(netcdf)]
    fn single_file_is_a_one_entry_catalog() {
        let dir = tempfile::tempdir().unwrap();
        let nc_path = dir.path().join("one.nc");
        process_to(ASSET_2022, &nc_path, Some("single-file-test"), None);

        let catalog = Catalog::discover(&nc_path);
        assert_eq!(catalog.entries.len(), 1);
        assert_eq!(catalog.entries[0].radargram_id.as_str(), "single-file-test");
        assert!(catalog.warnings.is_empty());
    }

    #[test]
    #[test_retry::retry]
    #[serial_test::serial(netcdf)]
    fn directory_is_scanned_recursively_with_deterministic_order() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("a/nested")).unwrap();
        std::fs::create_dir_all(dir.path().join("b")).unwrap();

        process_to(
            ASSET_2022,
            &dir.path().join("b/second.nc"),
            Some("dir-b-second"),
            None,
        );
        process_to(
            ASSET_2022,
            &dir.path().join("a/nested/first.nc"),
            Some("dir-a-nested-first"),
            None,
        );

        let catalog = Catalog::discover(dir.path());
        assert_eq!(catalog.entries.len(), 2);
        // Deterministic lexicographic order by relative path: "a/..." < "b/...".
        assert_eq!(catalog.entries[0].relative_path, "a/nested/first.nc");
        assert_eq!(catalog.entries[1].relative_path, "b/second.nc");
        assert!(catalog.warnings.is_empty());
    }

    #[test]
    #[test_retry::retry]
    #[serial_test::serial(netcdf)]
    fn nested_radargrams_with_recurring_filenames_remain_separate() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("x")).unwrap();
        std::fs::create_dir_all(dir.path().join("y")).unwrap();

        process_to(
            ASSET_2022,
            &dir.path().join("x/processed.nc"),
            Some("recurring-x"),
            None,
        );
        process_to(
            ASSET_2022,
            &dir.path().join("y/processed.nc"),
            Some("recurring-y"),
            None,
        );

        let catalog = Catalog::discover(dir.path());
        assert_eq!(catalog.entries.len(), 2);
        let ids: Vec<&str> = catalog
            .entries
            .iter()
            .map(|e| e.radargram_id.as_str())
            .collect();
        assert!(ids.contains(&"recurring-x"));
        assert!(ids.contains(&"recurring-y"));
    }

    #[test]
    #[test_retry::retry]
    #[serial_test::serial(netcdf)]
    fn duplicate_radargram_ids_resolve_to_the_newest() {
        let dir = tempfile::tempdir().unwrap();
        let older = dir.path().join("older.nc");
        let newer = dir.path().join("newer.nc");

        process_to(ASSET_2022, &older, Some("dup-id"), None);
        // A distinct processing_datetime is guaranteed because export.rs
        // stamps chrono::Local::now() -- but to make the "newest wins" rule
        // unambiguous rather than racing the clock, force older's datetime
        // backward directly in the file.
        {
            let mut f = netcdf::append(&older).unwrap();
            f.add_attribute("ridal_processing_datetime", "2000-01-01T00:00:00Z")
                .unwrap();
        }
        process_to(ASSET_2022, &newer, Some("dup-id"), None);

        let catalog = Catalog::discover(dir.path());
        assert_eq!(catalog.entries.len(), 1);
        assert_eq!(catalog.entries[0].relative_path, "newer.nc");
        assert_eq!(catalog.warnings.len(), 1);
        assert!(catalog.warnings[0]
            .message
            .contains("Duplicate radargram ID"));
    }

    #[test]
    #[test_retry::retry]
    #[serial_test::serial(netcdf)]
    fn duplicate_ids_equal_datetime_breaks_tie_by_path() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.nc");
        let z = dir.path().join("z.nc");
        process_to(ASSET_2022, &a, Some("dup-tie"), None);
        process_to(ASSET_2022, &z, Some("dup-tie"), None);

        // Force identical processing_datetime so the path-order tiebreak is
        // what's actually being exercised, not real clock timing.
        for p in [&a, &z] {
            let mut f = netcdf::append(p).unwrap();
            f.add_attribute("ridal_processing_datetime", "2020-01-01T00:00:00Z")
                .unwrap();
        }

        let catalog = Catalog::discover(dir.path());
        assert_eq!(catalog.entries.len(), 1);
        // "a.nc" sorts before "z.nc" lexicographically.
        assert_eq!(catalog.entries[0].relative_path, "a.nc");
    }

    #[test]
    #[test_retry::retry]
    #[serial_test::serial(netcdf)]
    fn one_unreadable_candidate_does_not_abort_discovery() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("garbage.nc"), b"not a netcdf file").unwrap();
        process_to(
            ASSET_2022,
            &dir.path().join("good.nc"),
            Some("good-one"),
            None,
        );

        let catalog = Catalog::discover(dir.path());
        assert_eq!(catalog.entries.len(), 1);
        assert_eq!(catalog.entries[0].radargram_id.as_str(), "good-one");
        assert_eq!(catalog.warnings.len(), 1);
    }

    #[test]
    fn unrelated_and_invalid_files_are_silently_ignored_not_warned() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("readme.txt"), b"not even nc").unwrap();

        let catalog = Catalog::discover(dir.path());
        assert!(catalog.entries.is_empty());
        assert!(catalog.warnings.is_empty());
    }

    #[test]
    fn excluded_directories_are_not_descended_into() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        std::fs::create_dir_all(dir.path().join(".hidden")).unwrap();
        std::fs::write(dir.path().join(".git/pretend.nc"), b"garbage").unwrap();
        std::fs::write(dir.path().join(".hidden/pretend.nc"), b"garbage").unwrap();

        let catalog = Catalog::discover(dir.path());
        // Nothing under excluded directories should even be attempted, so
        // there is no warning either -- discovery never saw the files.
        assert!(catalog.entries.is_empty());
        assert!(catalog.warnings.is_empty());
    }

    fn radargram(name: &str) -> RadargramId {
        RadargramId::new(name).unwrap()
    }

    fn group(name: &str) -> GroupId {
        GroupId::new(name).unwrap()
    }

    fn named(display: &str) -> crate::project::overrides::RadargramOverride {
        crate::project::overrides::RadargramOverride {
            display_name: DisplayName::from_input(display),
            ..Default::default()
        }
    }

    #[test]
    #[test_retry::retry]
    #[serial_test::serial(netcdf)]
    fn an_override_renames_a_radargram_without_touching_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.nc");
        process_to(ASSET_2022, &path, Some("line-01"), Some("Old group"));

        let mut overrides = CatalogOverrides::default();
        overrides
            .radargrams
            .insert(radargram("line-01"), named("A better name"));

        let catalog = Catalog::discover_with_overrides(dir.path(), &overrides);
        assert_eq!(catalog.entries[0].effective_label(), "A better name");
        // Only the label. Grouping is a separate field and was not set, so
        // it still comes from the file -- that is what "absent means
        // inherit, per field" buys.
        assert_eq!(
            catalog.entries[0].group_name.as_ref().map(|g| g.as_str()),
            Some("Old group")
        );

        // And the file itself is untouched, which is the whole premise.
        let plain = Catalog::discover(dir.path());
        assert_eq!(plain.entries[0].effective_label(), "line-01");
    }

    #[test]
    #[test_retry::retry]
    #[serial_test::serial(netcdf)]
    fn a_radargram_moved_between_groups_does_not_bring_its_old_name() {
        let dir = tempfile::tempdir().unwrap();
        process_to(
            ASSET_2022,
            &dir.path().join("a.nc"),
            Some("line-01"),
            Some("Kroppbreen 2022"),
        );

        let mut overrides = CatalogOverrides::default();
        overrides.radargrams.insert(
            radargram("line-01"),
            crate::project::overrides::RadargramOverride {
                group: Some(crate::project::overrides::GroupMembership::Group(group(
                    "dronbreen-2022",
                ))),
                ..Default::default()
            },
        );
        overrides.groups.insert(
            group("dronbreen-2022"),
            crate::project::overrides::GroupOverride {
                name: GroupName::from_input("Drønbreen 2022"),
            },
        );

        let catalog = Catalog::discover_with_overrides(dir.path(), &overrides);
        let entry = &catalog.entries[0];
        assert_eq!(
            entry.group_id.as_ref().map(|g| g.as_str()),
            Some("dronbreen-2022")
        );
        // The name in the file describes the group it was processed into,
        // and it has just been moved out of it. Carrying "Kroppbreen 2022"
        // along would make the card disagree with its own heading.
        assert_eq!(
            entry.group_name.as_ref().map(|g| g.as_str()),
            Some("Drønbreen 2022")
        );
        assert_eq!(
            catalog.group_names[&group("dronbreen-2022")].as_str(),
            "Drønbreen 2022"
        );
    }

    #[test]
    #[test_retry::retry]
    #[serial_test::serial(netcdf)]
    fn naming_a_group_settles_a_disagreement_instead_of_re_reporting_it() {
        // Two files in one group that disagree about its name. Without an
        // override the catalog picks a winner and says so; with one there
        // is nothing left to warn about, and repeating the warning would
        // send an operator to fix what they have already fixed.
        let dir = tempfile::tempdir().unwrap();
        process_to_with_group_id(
            ASSET_2022,
            &dir.path().join("a.nc"),
            Some("line-01"),
            "Kroppbreen 2022",
            "kroppbreen",
        );
        process_to_with_group_id(
            ASSET_2022,
            &dir.path().join("b.nc"),
            Some("line-02"),
            "Kroppbreen twentytwentytwo",
            "kroppbreen",
        );

        let plain = Catalog::discover(dir.path());
        assert_eq!(plain.warnings.len(), 1, "{:?}", plain.warnings);
        assert!(plain.warnings[0].message.contains("disagreeing names"));

        let mut overrides = CatalogOverrides::default();
        overrides.groups.insert(
            group("kroppbreen"),
            crate::project::overrides::GroupOverride {
                name: GroupName::from_input("Kroppbreen 2022"),
            },
        );
        let settled = Catalog::discover_with_overrides(dir.path(), &overrides);
        assert!(settled.warnings.is_empty(), "{:?}", settled.warnings);
        assert_eq!(
            settled.group_names[&group("kroppbreen")].as_str(),
            "Kroppbreen 2022"
        );
    }

    #[test]
    #[test_retry::retry]
    #[serial_test::serial(netcdf)]
    fn a_group_override_for_a_group_nothing_is_in_conjures_no_heading() {
        let dir = tempfile::tempdir().unwrap();
        process_to(ASSET_2022, &dir.path().join("a.nc"), Some("line-01"), None);

        let mut overrides = CatalogOverrides::default();
        overrides.groups.insert(
            group("abandoned"),
            crate::project::overrides::GroupOverride {
                name: GroupName::from_input("Nobody lives here"),
            },
        );

        let catalog = Catalog::discover_with_overrides(dir.path(), &overrides);
        assert!(
            !catalog.group_names.contains_key(&group("abandoned")),
            "a stale override must not put an empty group on the index"
        );
    }

    #[test]
    #[test_retry::retry]
    #[serial_test::serial(netcdf)]
    fn an_override_for_a_radargram_that_is_not_there_changes_nothing() {
        // Overrides outlive the radargrams they name -- a file gets moved
        // out of the catalog root and the document still mentions it.
        let dir = tempfile::tempdir().unwrap();
        process_to(ASSET_2022, &dir.path().join("a.nc"), Some("line-01"), None);

        let mut overrides = CatalogOverrides::default();
        overrides
            .radargrams
            .insert(radargram("long-gone"), named("A ghost"));

        let catalog = Catalog::discover_with_overrides(dir.path(), &overrides);
        assert_eq!(catalog.entries.len(), 1);
        assert_eq!(catalog.entries[0].effective_label(), "line-01");
        assert!(catalog.warnings.is_empty());
    }

    #[test]
    #[test_retry::retry]
    #[serial_test::serial(netcdf)]
    fn unlisted_is_carried_onto_the_entry() {
        let dir = tempfile::tempdir().unwrap();
        process_to(ASSET_2022, &dir.path().join("a.nc"), Some("line-01"), None);

        let mut overrides = CatalogOverrides::default();
        overrides.radargrams.insert(
            radargram("line-01"),
            crate::project::overrides::RadargramOverride {
                unlisted: true,
                ..Default::default()
            },
        );

        let catalog = Catalog::discover_with_overrides(dir.path(), &overrides);
        // Still discovered -- unlisted is about listings, not existence.
        assert_eq!(catalog.entries.len(), 1);
        assert!(catalog.entries[0].unlisted);
    }

    fn writable(path: &std::path::Path) -> CatalogRoot {
        CatalogRoot {
            path: path.to_path_buf(),
            is_file: false,
            writable: true,
            group_bases: Vec::new(),
        }
    }

    fn read_only(path: &std::path::Path) -> CatalogRoot {
        CatalogRoot {
            path: path.to_path_buf(),
            is_file: false,
            writable: false,
            group_bases: Vec::new(),
        }
    }

    #[test]
    #[test_retry::retry]
    #[serial_test::serial(netcdf)]
    fn the_project_wins_over_an_external_root_however_new_the_external_file_is() {
        // The rule the overlay rests on, and a real change from the
        // single-root behaviour. Without it an external file reprocessed
        // later would silently override a deliberate in-project decision,
        // and the project would not be an overlay at all -- it would just
        // be another directory in the pile.
        let project = tempfile::tempdir().unwrap();
        let archive = tempfile::tempdir().unwrap();
        process_to(
            ASSET_2022,
            &project.path().join("ours.nc"),
            Some("shared"),
            None,
        );
        process_to(
            ASSET_2022,
            &archive.path().join("theirs.nc"),
            Some("shared"),
            None,
        );

        // Make the external one unambiguously newer, which under the
        // datetime rule alone would win.
        {
            let mut f = netcdf::append(archive.path().join("theirs.nc")).unwrap();
            f.add_attribute("ridal_processing_datetime", "2099-01-01T00:00:00Z")
                .unwrap();
        }

        let roots = [writable(project.path()), read_only(archive.path())];
        let catalog = Catalog::discover_roots(&roots, &CatalogOverrides::default());
        assert_eq!(catalog.entries.len(), 1);
        assert_eq!(catalog.entries[0].root, 0, "the project's copy is served");
        assert_eq!(catalog.entries[0].relative_path, "ours.nc");
        // And it is not reported as a problem: the overlay winning is the
        // arrangement working.
        assert!(catalog.warnings.is_empty(), "{:?}", catalog.warnings);
    }

    #[test]
    #[test_retry::retry]
    #[serial_test::serial(netcdf)]
    fn two_external_roots_holding_one_id_is_still_a_duplicate() {
        // The overlay rule is about layers, not about silencing duplicates.
        // Two archives disagreeing is exactly what the warning is for, and
        // there is no deliberate decision behind it to respect.
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        process_to(ASSET_2022, &first.path().join("a.nc"), Some("shared"), None);
        process_to(
            ASSET_2022,
            &second.path().join("b.nc"),
            Some("shared"),
            None,
        );

        let roots = [read_only(first.path()), read_only(second.path())];
        let catalog = Catalog::discover_roots(&roots, &CatalogOverrides::default());
        assert_eq!(catalog.entries.len(), 1);
        assert_eq!(catalog.warnings.len(), 1, "{:?}", catalog.warnings);
        assert!(catalog.warnings[0]
            .message
            .contains("Duplicate radargram ID"));
    }

    #[test]
    #[test_retry::retry]
    #[serial_test::serial(netcdf)]
    fn an_ignored_radargram_is_not_served_and_comes_back_when_the_decision_is_lifted() {
        // Ignoring is how a radargram in a read-only archive is "removed":
        // Ridal never writes below the project, so the only thing it can
        // change is whether it serves the file.
        let archive = tempfile::tempdir().unwrap();
        process_to(
            ASSET_2022,
            &archive.path().join("a.nc"),
            Some("line-01"),
            None,
        );
        process_to(
            ASSET_2022,
            &archive.path().join("b.nc"),
            Some("line-02"),
            None,
        );
        let roots = [read_only(archive.path())];

        let mut overrides = CatalogOverrides::default();
        overrides.ignored.insert(
            radargram("line-02"),
            crate::project::overrides::IgnoredRadargram {
                since: Some("2026-09-13T00:00:00Z".to_string()),
                revision_id: None,
            },
        );

        let catalog = Catalog::discover_roots(&roots, &overrides);
        assert_eq!(
            catalog
                .entries
                .iter()
                .map(|e| e.radargram_id.as_str())
                .collect::<Vec<_>>(),
            vec!["line-01"]
        );
        assert!(
            catalog.vestigial_ignores().is_empty(),
            "the file is right there"
        );

        // Lifting the decision brings it back without re-reading the disk,
        // which is what putting the filter in resolution buys.
        let lifted = catalog.reresolved(&CatalogOverrides::default());
        assert_eq!(lifted.entries.len(), 2);
    }

    #[test]
    #[test_retry::retry]
    #[serial_test::serial(netcdf)]
    fn two_external_copies_still_warn_even_when_the_project_wins() {
        // The overlay branch answers "which one is served". It must not
        // also swallow the collision *between* the two that lost: there is
        // no deliberate decision behind that one, and it is what the
        // warning is for.
        let project = tempfile::tempdir().unwrap();
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        for (dir, name) in [
            (project.path(), "ours.nc"),
            (first.path(), "a.nc"),
            (second.path(), "b.nc"),
        ] {
            process_to(ASSET_2022, &dir.join(name), Some("shared"), None);
        }

        let roots = [
            writable(project.path()),
            read_only(first.path()),
            read_only(second.path()),
        ];
        let catalog = Catalog::discover_roots(&roots, &CatalogOverrides::default());
        assert_eq!(catalog.entries.len(), 1);
        assert_eq!(catalog.entries[0].root, 0, "the project's copy is served");
        assert_eq!(
            catalog.warnings.len(),
            1,
            "the two externals collide and that is worth saying: {:?}",
            catalog.warnings
        );
        let message = &catalog.warnings[0].message;
        assert!(message.contains("Duplicate radargram ID"), "{message}");
        assert!(
            message.contains("a.nc") && message.contains("b.nc"),
            "{message}"
        );
        assert!(
            !message.contains("ours.nc"),
            "the project's copy is not part of that collision: {message}"
        );
    }

    #[test]
    #[test_retry::retry]
    #[serial_test::serial(netcdf)]
    fn an_ignored_radargram_whose_file_changed_says_so_without_un_ignoring_it() {
        // The decision is on the id, so a replaced file stays ignored. But
        // something deliberately not shown becoming a *different* thing
        // deliberately not shown is exactly when whoever decided would want
        // to look again -- and the recorded revision was otherwise inert,
        // stored on the way in and never compared to anything.
        let archive = tempfile::tempdir().unwrap();
        process_to(
            ASSET_2022,
            &archive.path().join("a.nc"),
            Some("line-01"),
            None,
        );

        let mut overrides = CatalogOverrides::default();
        overrides.ignored.insert(
            radargram("line-01"),
            crate::project::overrides::IgnoredRadargram {
                since: Some("2026-09-13T00:00:00Z".to_string()),
                revision_id: Some("a-different-revision".to_string()),
            },
        );

        let catalog = Catalog::discover_roots(&[read_only(archive.path())], &overrides);
        assert!(catalog.entries.is_empty(), "still not served");
        let changed: Vec<_> = catalog
            .warnings
            .iter()
            .filter(|w| w.message.contains("has changed since"))
            .collect();
        assert_eq!(changed.len(), 1, "{:?}", catalog.warnings);
        assert!(changed[0].operator_only, "only the person who decided");
        assert!(
            changed[0].about.is_empty(),
            "naming it would hide the warning from everyone, since an \
             ignored radargram is in nobody's listing"
        );

        // And when it has not changed, nothing is said.
        let current = catalog.entries.first().map(|e| e.revision_id.to_string());
        let _ = current;
        let mut matching = CatalogOverrides::default();
        let plain = Catalog::discover_roots(&[read_only(archive.path())], &matching);
        let revision = plain.entries[0].revision_id.to_string();
        matching.ignored.insert(
            radargram("line-01"),
            crate::project::overrides::IgnoredRadargram {
                since: Some("2026-09-13T00:00:00Z".to_string()),
                revision_id: Some(revision),
            },
        );
        let quiet = Catalog::discover_roots(&[read_only(archive.path())], &matching);
        assert!(
            !quiet
                .warnings
                .iter()
                .any(|w| w.message.contains("has changed since")),
            "{:?}",
            quiet.warnings
        );
    }

    #[test]
    #[test_retry::retry]
    #[serial_test::serial(netcdf)]
    fn an_ignore_with_nothing_to_act_on_is_listed_rather_than_forgotten() {
        // An external root can be unmounted for a week and come back.
        // Clearing the decision because the disk was busy would quietly
        // start serving something somebody chose not to serve -- so it is
        // kept, and made visible instead of accumulating silently.
        let archive = tempfile::tempdir().unwrap();
        process_to(
            ASSET_2022,
            &archive.path().join("a.nc"),
            Some("line-01"),
            None,
        );

        let mut overrides = CatalogOverrides::default();
        overrides.ignored.insert(
            radargram("long-gone"),
            crate::project::overrides::IgnoredRadargram {
                since: Some("2026-01-01T00:00:00Z".to_string()),
                revision_id: None,
            },
        );

        let catalog = Catalog::discover_roots(&[read_only(archive.path())], &overrides);
        let vestigial = catalog.vestigial_ignores();
        assert_eq!(vestigial.len(), 1);
        assert_eq!(vestigial[0].radargram_id.as_str(), "long-gone");
        assert_eq!(vestigial[0].since.as_deref(), Some("2026-01-01T00:00:00Z"));
        // Still ignored, not quietly dropped.
        assert_eq!(catalog.entries.len(), 1);
    }

    #[test]
    #[test_retry::retry]
    #[serial_test::serial(netcdf)]
    fn renaming_a_group_renames_it_on_every_member_that_was_already_in_it() {
        // The case an earlier version got wrong, and the one my own group
        // test missed: those radargrams were in the group *because* of an
        // override, so their file name had already been cleared and the
        // gap-filling happened to cover them. A radargram whose file puts
        // it in the group keeps its own name unless every member is
        // assigned the resolved one.
        let dir = tempfile::tempdir().unwrap();
        process_to_with_group_id(
            ASSET_2022,
            &dir.path().join("a.nc"),
            Some("line-01"),
            "Kroppbreen",
            "kroppbreen",
        );

        let plain = Catalog::discover(dir.path());
        assert_eq!(
            plain.entries[0].group_name.as_ref().map(|g| g.as_str()),
            Some("Kroppbreen"),
            "the file's own name, with no overrides"
        );

        let mut overrides = CatalogOverrides::default();
        overrides.groups.insert(
            group("kroppbreen"),
            crate::project::overrides::GroupOverride {
                name: GroupName::from_input("Kroppbreen, spring 2022"),
            },
        );

        let renamed = Catalog::discover_with_overrides(dir.path(), &overrides);
        assert_eq!(
            renamed.entries[0].group_name.as_ref().map(|g| g.as_str()),
            Some("Kroppbreen, spring 2022"),
            "the card must not disagree with the heading above it"
        );
        // And the provenance is still there for anyone who wants it.
        assert_eq!(
            renamed.entries[0]
                .from_file
                .group_name
                .as_ref()
                .map(|g| g.as_str()),
            Some("Kroppbreen")
        );
    }

    #[test]
    #[test_retry::retry]
    #[serial_test::serial(netcdf)]
    fn re_resolving_gives_what_rediscovery_would_have() {
        // The property that lets a label edit skip the disk. If these ever
        // disagree, the cheap path is quietly serving something a restart
        // would not.
        let dir = tempfile::tempdir().unwrap();
        process_to_with_group_id(
            ASSET_2022,
            &dir.path().join("a.nc"),
            Some("line-01"),
            "Kroppbreen 2022",
            "kroppbreen",
        );
        process_to_with_group_id(
            ASSET_2022,
            &dir.path().join("b.nc"),
            Some("line-02"),
            "Kroppbreen 2022",
            "kroppbreen",
        );

        let mut overrides = CatalogOverrides::default();
        overrides
            .radargrams
            .insert(radargram("line-01"), named("A better name"));
        overrides.radargrams.insert(
            radargram("line-02"),
            crate::project::overrides::RadargramOverride {
                group: Some(crate::project::overrides::GroupMembership::Group(group(
                    "dronbreen-2022",
                ))),
                unlisted: true,
                ..Default::default()
            },
        );
        overrides.groups.insert(
            group("dronbreen-2022"),
            crate::project::overrides::GroupOverride {
                name: GroupName::from_input("Drønbreen 2022"),
            },
        );

        let discovered = Catalog::discover_with_overrides(dir.path(), &overrides);
        // Start from a catalog that knows nothing of the overrides, the way
        // the server does before anyone edits anything.
        let reresolved = Catalog::discover(dir.path()).reresolved(&overrides);

        assert_eq!(reresolved.entries, discovered.entries);
        assert_eq!(reresolved.group_names, discovered.group_names);
        assert_eq!(reresolved.warnings, discovered.warnings);

        // And re-resolving back to nothing returns the plain catalog, so an
        // override cannot leave a residue in the cheap path.
        let plain = Catalog::discover(dir.path());
        let reverted = reresolved.reresolved(&CatalogOverrides::default());
        assert_eq!(reverted.entries, plain.entries);
        assert_eq!(reverted.group_names, plain.group_names);
        assert_eq!(reverted.warnings, plain.warnings);
    }

    #[test]
    fn effective_label_prefers_display_name() {
        let entry = CatalogEntry {
            radargram_id: RadargramId::new("kroppbreen-01").unwrap(),
            revision_id: RevisionId::fingerprint_v1(
                &RadargramId::new("kroppbreen-01").unwrap(),
                "2020-01-01T00:00:00Z",
            ),
            display_name: DisplayName::from_input("Kroppbreen line 1"),
            group_name: None,
            group_id: None,
            processing_datetime: "2020-01-01T00:00:00Z".to_string(),
            shape: (10, 10),
            relative_path: "a.nc".to_string(),
            root: 0,
            unlisted: false,
            elevation_min: None,
            elevation_max: None,
            from_file: FileMetadata::default(),
        };
        assert_eq!(entry.effective_label(), "Kroppbreen line 1");

        let mut no_name = entry.clone();
        no_name.display_name = None;
        assert_eq!(no_name.effective_label(), "kroppbreen-01");
    }

    #[test]
    fn revision_id_is_deterministic_and_ignores_path() {
        let id = RadargramId::new("a").unwrap();
        let a = RevisionId::fingerprint_v1(&id, "2020-01-01T00:00:00Z");
        let b = RevisionId::fingerprint_v1(&id, "2020-01-01T00:00:00Z");
        assert_eq!(a, b);

        let different_time = RevisionId::fingerprint_v1(&id, "2021-01-01T00:00:00Z");
        assert_ne!(a, different_time);

        let other_id = RadargramId::new("b").unwrap();
        let different_id = RevisionId::fingerprint_v1(&other_id, "2020-01-01T00:00:00Z");
        assert_ne!(a, different_id);
    }

    #[test]
    #[test_retry::retry]
    #[serial_test::serial(netcdf)]
    fn group_falls_back_to_parent_directory() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("dronbreen/2022")).unwrap();
        process_to(
            ASSET_2022,
            &dir.path().join("dronbreen/2022/a.nc"),
            Some("group-fallback-test"),
            None, // no explicit --group-name
        );

        let catalog = Catalog::discover(dir.path());
        assert_eq!(catalog.entries.len(), 1);
        assert_eq!(
            catalog.entries[0].group_id.as_ref().map(|g| g.as_str()),
            Some("dronbreen-2022")
        );
        assert_eq!(
            catalog.entries[0].group_name.as_ref().map(|g| g.as_str()),
            Some("dronbreen/2022")
        );
    }

    #[test]
    #[test_retry::retry]
    #[serial_test::serial(netcdf)]
    fn a_projects_own_radargram_directory_is_not_a_group() {
        // Serving a project root puts every file in `radargrams/` one level
        // down, so the parent-directory fallback made a group called
        // "radargrams" -- a name nobody chose, from a directory that only
        // exists because a project has to keep its files somewhere. Adding
        // a radargram through the GUI is how you meet this: it lands there
        // by definition.
        let dir = tempfile::tempdir().unwrap();
        let radargrams = dir.path().join("radargrams");
        std::fs::create_dir_all(radargrams.join("dronbreen/2022")).unwrap();
        process_to(
            ASSET_2022,
            &radargrams.join("flat.nc"),
            Some("flat-in-the-project"),
            None,
        );
        process_to(
            ASSET_2022,
            &radargrams.join("dronbreen/2022/nested.nc"),
            Some("nested-in-the-project"),
            None,
        );

        let root = CatalogRoot {
            path: dir.path().to_path_buf(),
            is_file: false,
            writable: true,
            group_bases: vec![radargrams.clone()],
        };
        let catalog = Catalog::discover_roots(&[root], &CatalogOverrides::default());

        let group_of = |id: &str| {
            catalog
                .entries
                .iter()
                .find(|e| e.radargram_id.as_str() == id)
                .unwrap_or_else(|| panic!("{id} is not in the catalog"))
                .group_id
                .as_ref()
                .map(|g| g.as_str().to_string())
        };
        assert_eq!(
            group_of("flat-in-the-project"),
            None,
            "structure, not a group"
        );
        // And the fallback still does its job below that: an archive laid
        // out by survey and year is exactly what it is for.
        assert_eq!(
            group_of("nested-in-the-project"),
            Some("dronbreen-2022".to_string())
        );
    }

    #[test]
    #[test_retry::retry]
    #[serial_test::serial(netcdf)]
    fn explicit_group_wins_over_directory_fallback() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("some/deep/path")).unwrap();
        process_to(
            ASSET_2022,
            &dir.path().join("some/deep/path/a.nc"),
            Some("explicit-group-test"),
            Some("real-group"),
        );

        let catalog = Catalog::discover(dir.path());
        assert_eq!(
            catalog.entries[0].group_name.as_ref().map(|g| g.as_str()),
            Some("real-group")
        );
        assert_eq!(
            catalog.entries[0].group_id.as_ref().map(|g| g.as_str()),
            Some("real-group")
        );
    }

    #[test]
    #[test_retry::retry]
    #[serial_test::serial(netcdf)]
    fn unicode_group_name_derives_an_ascii_id() {
        let dir = tempfile::tempdir().unwrap();
        let nc_path = dir.path().join("a.nc");
        process_to(
            ASSET_2022,
            &nc_path,
            Some("unicode-group-test"),
            Some("Drønbreen"),
        );

        let catalog = Catalog::discover(dir.path());
        assert_eq!(
            catalog.entries[0].group_name.as_ref().map(|g| g.as_str()),
            Some("Drønbreen")
        );
        assert_eq!(
            catalog.entries[0].group_id.as_ref().map(|g| g.as_str()),
            Some("dronbreen")
        );
        assert_eq!(
            catalog
                .group_names
                .get(catalog.entries[0].group_id.as_ref().unwrap())
                .map(|n| n.as_str()),
            Some("Drønbreen")
        );
    }

    #[test]
    #[test_retry::retry]
    #[serial_test::serial(netcdf)]
    fn explicit_group_id_overrides_derivation_from_name() {
        let dir = tempfile::tempdir().unwrap();
        let nc_path = dir.path().join("a.nc");
        process_to_with_group_id(
            ASSET_2022,
            &nc_path,
            Some("explicit-id-test"),
            "Drønbreen",
            "db",
        );

        let catalog = Catalog::discover(dir.path());
        assert_eq!(
            catalog.entries[0].group_name.as_ref().map(|g| g.as_str()),
            Some("Drønbreen")
        );
        assert_eq!(
            catalog.entries[0].group_id.as_ref().map(|g| g.as_str()),
            Some("db")
        );
    }

    #[test]
    #[test_retry::retry]
    #[serial_test::serial(netcdf)]
    fn disagreeing_group_names_pick_the_newest_and_warn() {
        let dir = tempfile::tempdir().unwrap();
        let older = dir.path().join("older.nc");
        let newer = dir.path().join("newer.nc");
        process_to_with_group_id(
            ASSET_2022,
            &older,
            Some("older-member"),
            "Old Name",
            "shared-id",
        );
        process_to_with_group_id(
            ASSET_2022,
            &newer,
            Some("newer-member"),
            "New Name",
            "shared-id",
        );
        // Force a deterministic ordering, matching the duplicate-id test's
        // own approach: real clock timing is not what's being exercised.
        {
            let mut f = netcdf::append(&older).unwrap();
            f.add_attribute("ridal_processing_datetime", "2020-01-01T00:00:00Z")
                .unwrap();
        }
        {
            let mut f = netcdf::append(&newer).unwrap();
            f.add_attribute("ridal_processing_datetime", "2021-01-01T00:00:00Z")
                .unwrap();
        }

        let catalog = Catalog::discover(dir.path());
        let id = GroupId::new("shared-id").unwrap();
        assert_eq!(
            catalog.group_names.get(&id).map(|n| n.as_str()),
            Some("New Name")
        );
        assert!(
            catalog
                .warnings
                .iter()
                .any(|w| w.message.contains("disagreeing names")),
            "{:?}",
            catalog.warnings
        );
    }
}
