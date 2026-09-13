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
    /// Set by a project override (#145). Curation, not access control: an
    /// unlisted radargram is left out of listings and still reachable by
    /// anyone who knows its id.
    pub unlisted: bool,
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
}

/// A non-fatal problem encountered during discovery: an unreadable
/// candidate, an inspection error, or a duplicate radargram ID collision.
#[derive(Debug, Clone, PartialEq)]
pub struct CatalogWarning {
    pub message: String,
}

/// The result of discovering radargrams under one root (a file or a
/// directory).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Catalog {
    pub entries: Vec<CatalogEntry>,
    pub warnings: Vec<CatalogWarning>,
    /// One representative display name per group id, for the index page
    /// (a group has one heading, even though every entry carries its own
    /// `group_name` for provenance). When entries sharing a `group_id`
    /// disagree on the name, resolved exactly like a duplicate
    /// `radargram_id` (#122): most recent `processing_datetime` wins, ties
    /// broken by path order, with a `CatalogWarning` either way.
    pub group_names: std::collections::BTreeMap<GroupId, GroupName>,
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
}

fn discover_candidates(root: &Path) -> Vec<Candidate> {
    if root.is_file() {
        let name = root
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        return vec![Candidate {
            path: root.to_path_buf(),
            relative_path: name,
            group_hint: None,
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
        let group_hint = relative
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
        let candidates = discover_candidates(root);
        let mut warnings = Vec::new();
        let mut by_id: std::collections::BTreeMap<String, (CatalogEntry, String)> =
            std::collections::BTreeMap::new();

        for candidate in candidates {
            let inspection = match io::inspect_ridal_netcdf(&candidate.path) {
                Ok(k) => k,
                Err(e) => {
                    warnings.push(CatalogWarning {
                        message: format!("{}: {e}", candidate.relative_path),
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
                from_file: FileMetadata {
                    display_name: display_name.clone(),
                    group_name: group_name.clone(),
                    group_id: group_id.clone(),
                },
            };

            let id_key = meta.radargram_id.as_str().to_string();
            match by_id.get(&id_key) {
                None => {
                    by_id.insert(id_key, (entry, candidate.relative_path));
                }
                Some((existing, existing_path)) => {
                    // Resolve deterministically (#122): most recent
                    // ridal_processing_datetime wins; ties break by
                    // relative path sorting first. Exact copies (identical
                    // datetime and content) still produce a warning even
                    // though the "selected" entry is unambiguous, so the
                    // user is nudged toward assigning unique IDs.
                    let new_is_newer = entry.processing_datetime > existing.processing_datetime;
                    let tie_new_wins = entry.processing_datetime == existing.processing_datetime
                        && candidate.relative_path < *existing_path;

                    warnings.push(CatalogWarning {
                        message: format!(
                            "Duplicate radargram ID '{id_key}': '{existing_path}' and \
                             '{}'. Selected the entry with the most recent processing \
                             datetime{}.",
                            candidate.relative_path,
                            if entry.processing_datetime == existing.processing_datetime {
                                " (datetimes equal; broke the tie by path order)"
                            } else {
                                ""
                            }
                        ),
                    });

                    if new_is_newer || tie_new_wins {
                        by_id.insert(id_key, (entry, candidate.relative_path));
                    }
                }
            }
        }

        let mut entries: Vec<CatalogEntry> = by_id.into_values().map(|(e, _)| e).collect();
        entries.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));

        for entry in &mut entries {
            apply_override(entry, overrides);
        }

        let (group_names, group_warnings) = resolve_group_names(&entries, overrides);
        warnings.extend(group_warnings);

        // A radargram moved into another group has no name of its own for
        // it, since the one in its file describes the group it was
        // processed into. Fill that from the resolved name so the API and
        // the index cannot disagree about what group a card is in.
        for entry in &mut entries {
            if entry.group_name.is_none() {
                if let Some(id) = &entry.group_id {
                    entry.group_name = group_names.get(id).cloned();
                }
            }
        }

        Catalog {
            entries,
            warnings,
            group_names,
        }
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
    let mut group_name_state: std::collections::BTreeMap<GroupId, (GroupName, String, String)> =
        std::collections::BTreeMap::new();
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
                    ),
                );
            }
            Some((existing_name, existing_dt, existing_path)) => {
                if existing_name != name {
                    let new_is_newer = entry.processing_datetime > *existing_dt;
                    let tie_new_wins = entry.processing_datetime == *existing_dt
                        && entry.relative_path < *existing_path;

                    warnings.push(CatalogWarning {
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
                            ),
                        );
                    }
                }
            }
        }
    }

    let mut group_names: std::collections::BTreeMap<GroupId, GroupName> = group_name_state
        .into_iter()
        .map(|(id, (name, _, _))| (id, name))
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
            unlisted: false,
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
