//! Moving a pre-#187 project into `ridal_data/`.
//!
//! Before #187, `ridal project init` left eleven entries lying directly in
//! the project directory. Those projects exist -- on field laptops, in the
//! author's own test directories -- and the layout is what tells Ridal
//! where the interpretations are, so opening one with this Ridal would
//! report an empty project and start writing a second set of picks beside
//! the first. [`super::Project::open`] refuses instead, and points here.
//!
//! # What this moves, and what it deliberately does not
//!
//! Only entries Ridal itself created. `radargrams/` stays where it is: it
//! holds the user's own processed files, `[radargrams] roots` already
//! points at it, and relative paths in `ridal.toml` resolve the same way
//! they did before -- so leaving it alone is not a loose end, it is the
//! whole reason the marker stayed at the root. An explicitly configured
//! `[cache] dir` is left alone for the same reason.
//!
//! # Why a plan and then an application
//!
//! Moving somebody's interpretations is the one operation here that cannot
//! be undone by re-running it. Building the plan first means `--dry-run`
//! prints exactly what the real run will do, and that every refusal
//! (something already at the destination, a marker that will not parse)
//! happens before anything has moved.

use std::path::{Path, PathBuf};

use super::{ProjectConfig, ProjectError, FORMAT_VERSION, MARKER};

/// Everything the pre-#187 layout kept in the project root, in the order a
/// person would want to read it.
///
/// `cache/` and `.staging/` are in here even though they are regenerable:
/// leaving them behind would mean the clutter this change is about stays in
/// the directory, and a stale `cache/` beside a `ridal_data/cache/` is a
/// question someone will have to answer later.
const MOVABLE: [&str; 11] = [
    "interpretations",
    "layers",
    "revisions",
    "preferences",
    "revisions.json",
    "overrides.json",
    "audit.json",
    "users.json",
    "session.key",
    "cache",
    ".staging",
];

/// What a migration would do.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub root: PathBuf,
    pub data_dir: PathBuf,
    /// Entry names to move from the root into the data directory.
    pub moves: Vec<String>,
    /// Whether `ridal_data/.gitignore` is still to be written.
    pub write_gitignore: bool,
}

impl Plan {
    /// Nothing to move and nothing to write: the project is already in the
    /// current layout.
    pub fn is_empty(&self) -> bool {
        self.moves.is_empty() && !self.write_gitignore
    }
}

/// Work out what moving `root` into its data directory would involve.
///
/// Refuses rather than merges when an entry exists in both places: that is
/// either a half-finished migration or two projects' state in one
/// directory, and picking a winner silently is how the losing copy is
/// discovered months later.
pub fn plan(root: &Path) -> Result<Plan, ProjectError> {
    let marker = root.join(MARKER);
    if !marker.is_file() {
        return Err(ProjectError::NotAProject(root.to_path_buf()));
    }
    let text = std::fs::read_to_string(&marker).map_err(|e| ProjectError::Io {
        path: marker.clone(),
        message: e.to_string(),
    })?;
    let config: ProjectConfig = toml::from_str(&text).map_err(|e| ProjectError::Config {
        path: marker.clone(),
        message: e.to_string(),
    })?;
    if let Some(version) = config.project.format_version {
        if version > FORMAT_VERSION {
            return Err(ProjectError::UnsupportedFormat {
                path: marker,
                version,
            });
        }
    }

    let data_dir = super::data_dir_of(root, &config);
    let mut moves = Vec::new();
    for entry in MOVABLE {
        // A configured cache is wherever the project put it, and that path
        // still resolves. Only the default location is ours to move.
        if entry == "cache" && config.cache.dir.is_some() {
            continue;
        }
        let from = root.join(entry);
        if !from.exists() {
            continue;
        }
        let to = data_dir.join(entry);
        if to.exists() {
            return Err(ProjectError::Io {
                path: to.clone(),
                message: format!(
                    "both {} and {} exist. Ridal will not guess which one is \
                     current -- move or delete one of them and run this again.",
                    from.display(),
                    to.display()
                ),
            });
        }
        moves.push(entry.to_string());
    }

    Ok(Plan {
        root: root.to_path_buf(),
        data_dir: data_dir.clone(),
        moves,
        write_gitignore: !data_dir.join(".gitignore").exists(),
    })
}

/// Carry out `plan`, then record the layout version in `ridal.toml`.
///
/// The version is written last, so an interrupted migration leaves a
/// project that still refuses to open and can be re-run, rather than one
/// that claims to be migrated with half its state in the old place.
pub fn apply(plan: &Plan) -> Result<(), ProjectError> {
    std::fs::create_dir_all(&plan.data_dir).map_err(|e| ProjectError::Io {
        path: plan.data_dir.clone(),
        message: e.to_string(),
    })?;

    for entry in &plan.moves {
        let from = plan.root.join(entry);
        let to = plan.data_dir.join(entry);
        // A rename, not a copy: it is atomic, it cannot run the disk out
        // half way through a season's interpretations, and the two are on
        // the same filesystem in every default layout. A data directory
        // configured onto another filesystem fails here with the system's
        // own message rather than silently duplicating gigabytes.
        std::fs::rename(&from, &to).map_err(|e| ProjectError::Io {
            path: from,
            message: format!("could not move it to {}: {e}", to.display()),
        })?;
    }

    if plan.write_gitignore {
        super::write_data_gitignore(&plan.data_dir)?;
    }

    record_format_version(&plan.root)
}

/// Add `format_version` to `[project]`, keeping the rest of the file.
///
/// `toml_edit` rather than a re-serialize, on the same terms as every other
/// write to the marker: the file is meant to be hand-edited, and a
/// migration that ate the user's comments would be a poor trade for one
/// key.
fn record_format_version(root: &Path) -> Result<(), ProjectError> {
    let marker = root.join(MARKER);
    let text = std::fs::read_to_string(&marker).map_err(|e| ProjectError::Io {
        path: marker.clone(),
        message: e.to_string(),
    })?;
    let mut document: toml_edit::DocumentMut =
        text.parse()
            .map_err(|e: toml_edit::TomlError| ProjectError::Config {
                path: marker.clone(),
                message: e.to_string(),
            })?;
    if !document.contains_key("project") {
        document["project"] = toml_edit::Item::Table(toml_edit::Table::new());
    }
    document["project"]["format_version"] = toml_edit::value(i64::from(FORMAT_VERSION));
    std::fs::write(&marker, document.to_string()).map_err(|e| ProjectError::Io {
        path: marker,
        message: e.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::Project;

    /// A project in the layout Ridal wrote before #187.
    fn legacy_project(root: &Path) {
        std::fs::create_dir_all(root.join("interpretations/line-01")).unwrap();
        std::fs::create_dir_all(root.join("layers")).unwrap();
        std::fs::create_dir_all(root.join("radargrams")).unwrap();
        std::fs::create_dir_all(root.join("cache")).unwrap();
        std::fs::write(
            root.join("interpretations/line-01/erik.gprinterp.json"),
            "{}",
        )
        .unwrap();
        std::fs::write(root.join("layers/layers.json"), "{}").unwrap();
        std::fs::write(root.join("radargrams/line-01.nc"), "not really").unwrap();
        std::fs::write(root.join("overrides.json"), "{}").unwrap();
        std::fs::write(
            root.join(MARKER),
            "# a note the user wrote\n\
             [project]\n\
             name = \"Old survey\"\n\
             \n\
             [radargrams]\n\
             roots = [\"radargrams\"]\n",
        )
        .unwrap();
    }

    #[test]
    fn a_legacy_project_is_refused_rather_than_read_as_empty() {
        // The failure this exists to prevent: opening it, finding no
        // interpretations where the new layout keeps them, and reporting a
        // project that is actually full of work as empty.
        let dir = tempfile::tempdir().unwrap();
        legacy_project(dir.path());

        let error = Project::open(dir.path()).unwrap_err().to_string();
        assert!(error.contains("older Ridal"), "{error}");
        assert!(error.contains("interpretations"), "{error}");
        assert!(error.contains("ridal project migrate"), "{error}");
    }

    #[test]
    fn migrating_moves_the_state_and_leaves_the_users_own_files_alone() {
        let dir = tempfile::tempdir().unwrap();
        legacy_project(dir.path());

        let plan = plan(dir.path()).unwrap();
        apply(&plan).unwrap();

        let data = dir.path().join("ridal_data");
        assert_eq!(
            std::fs::read_to_string(data.join("interpretations/line-01/erik.gprinterp.json"))
                .unwrap(),
            "{}"
        );
        assert!(data.join("layers/layers.json").is_file());
        assert!(data.join("overrides.json").is_file());
        assert!(data.join("cache").is_dir());
        assert!(data.join(".gitignore").is_file());
        assert!(!dir.path().join("interpretations").exists());
        assert!(!dir.path().join("overrides.json").exists());

        // The radargrams are the user's, not Ridal's, and `[radargrams]
        // roots` still points at them.
        assert!(dir.path().join("radargrams/line-01.nc").is_file());

        // And the project opens, with its comments and its name intact.
        let project = Project::open(dir.path()).unwrap();
        assert_eq!(project.config().project.name.as_deref(), Some("Old survey"));
        assert_eq!(project.data_dir(), data);
        let marker = std::fs::read_to_string(dir.path().join(MARKER)).unwrap();
        assert!(marker.contains("# a note the user wrote"), "{marker}");
        assert!(marker.contains("format_version = 1"), "{marker}");

        let roots = project.radargram_roots();
        assert_eq!(roots, vec![dir.path().join("radargrams")]);
    }

    #[test]
    fn migrating_an_already_migrated_project_has_nothing_to_do() {
        let dir = tempfile::tempdir().unwrap();
        Project::init(dir.path(), None).unwrap();

        let plan = plan(dir.path()).unwrap();
        assert!(plan.is_empty(), "{plan:?}");
        // And running it anyway is harmless.
        apply(&plan).unwrap();
        Project::open(dir.path()).unwrap();
    }

    #[test]
    fn a_collision_is_refused_before_anything_moves() {
        // Half a migration, or two projects' state in one directory. Either
        // way, choosing a winner silently is how the loser is discovered
        // months later.
        let dir = tempfile::tempdir().unwrap();
        legacy_project(dir.path());
        std::fs::create_dir_all(dir.path().join("ridal_data/layers")).unwrap();

        let error = plan(dir.path()).unwrap_err().to_string();
        assert!(error.contains("will not guess"), "{error}");
        // Nothing moved: the plan failed before it was applied.
        assert!(dir.path().join("interpretations").exists());
    }

    #[test]
    fn a_configured_cache_is_left_where_the_project_put_it() {
        let dir = tempfile::tempdir().unwrap();
        legacy_project(dir.path());
        let elsewhere = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(MARKER),
            format!(
                "[project]\n[cache]\ndir = \"{}\"\n",
                elsewhere.path().display()
            ),
        )
        .unwrap();

        let plan = plan(dir.path()).unwrap();
        assert!(!plan.moves.contains(&"cache".to_string()), "{plan:?}");
        apply(&plan).unwrap();
        // The old default-location cache stays put rather than being
        // shuffled into a directory nothing reads.
        assert!(dir.path().join("cache").is_dir());
        assert_eq!(
            Project::open(dir.path()).unwrap().cache_dir(),
            elsewhere.path()
        );
    }

    #[test]
    fn a_project_from_the_future_is_refused_rather_than_migrated() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(MARKER), "[project]\nformat_version = 99\n").unwrap();

        let error = plan(dir.path()).unwrap_err().to_string();
        assert!(error.contains("format_version = 99"), "{error}");
        let error = Project::open(dir.path()).unwrap_err().to_string();
        assert!(error.contains("Upgrade Ridal"), "{error}");
    }
}
