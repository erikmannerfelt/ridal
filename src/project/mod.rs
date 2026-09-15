//! Ridal projects: the on-disk home for everything that is *authored*
//! rather than processed.
//!
//! Until now Ridal has been strictly read-only, so a radargram's path was
//! all the state there was. Interpretations change that: picks have to be
//! saved somewhere, and so do the layer definitions they refer to. A project
//! is that somewhere.
//!
//! ```text
//! myproject/
//!   ridal.toml                              marker and settings
//!   radargrams/                             optional; roots are configurable
//!     dronbreen-0237.nc
//!   interpretations/
//!     dronbreen-0237/
//!       default.gprinterp.json              one document per user
//!   layers/
//!     layers.json                           project-scoped layer vocabulary
//!   users.json                              accounts and access policy (0600)
//!   session.key                             signs session cookies (0600)
//!   preferences/
//!     erik.json                             one person's viewing preferences
//!   cache/                                  derived data; safe to delete
//! ```
//!
//! `users.json` and `session.key` are absent until a project opts into
//! authentication, which is what keeps every project that predates it
//! working exactly as it did (#131).
//!
//! # Why an explicit marker
//!
//! A directory is a project only if it contains `ridal.toml`. Pointing
//! `ridal gui` at a bare directory of `.nc` files keeps working exactly as
//! before, read-only, which means adding a write path takes nothing away
//! from the existing behaviour. It also makes "where do these picks go?"
//! answerable by looking, rather than by inference from what happens to be
//! lying around.
//!
//! # Two kinds of data, deliberately separated
//!
//! Everything under `interpretations/` and `layers/` is **authored**: a
//! person made it, nothing can regenerate it, and losing it is data loss.
//! Those go through [`store::DocumentStore`], which writes atomically and
//! refuses to silently overwrite a concurrent edit.
//!
//! `cache/` is **derived**: rendered images and anything else Ridal can
//! rebuild from a radargram. It is deliberately *not* a document store.
//! Deleting it must always be safe, it needs no versioning or conflict
//! detection, and it should not be backed up -- which is why Ridal drops a
//! `CACHEDIR.TAG` in it, the convention backup tools already understand.
//! Its location is configurable precisely because a project directory may
//! sit on a network share while a cache wants local disk, which is the
//! normal arrangement for a long-running daemon.
//!
//! # Adding another kind of data later
//!
//! Give it a store directory and a module beside [`interpretations`] and
//! [`layers`]. The document store knows nothing about schemas, so nothing in
//! `store.rs` needs to change.

pub mod audit;
pub mod basemaps;
pub mod interpretations;
pub mod layers;
pub mod overlays;
pub mod overrides;
pub mod preferences;
pub mod revisions;
pub mod store;
pub mod users;

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use store::DocumentStore;

/// The file whose presence makes a directory a project.
pub const MARKER: &str = "ridal.toml";

/// Store directory for interpretations, relative to the project root.
pub const INTERPRETATIONS_DIR: &str = "interpretations";
/// Store directory for layer definitions.
pub const LAYERS_DIR: &str = "layers";
/// Store directory for per-user viewing preferences.
pub const PREFERENCES_DIR: &str = "preferences";
/// Default location for derived data.
pub const DEFAULT_CACHE_DIR: &str = "cache";

/// How large a project may grow before uploads are refused, unless the
/// project says otherwise.
///
/// 50 GB: roomy enough that a survey season does not hit it by accident,
/// small enough that a runaway client cannot quietly fill a shared disk
/// before anyone notices. The number matters less than there being one --
/// an unbounded upload endpoint is the kind of thing that is fine until it
/// is not.
pub const DEFAULT_MAX_PROJECT_BYTES: u64 = 50 * 1024 * 1024 * 1024;
/// Default directory scanned for radargrams when the config says nothing.
pub const DEFAULT_RADARGRAM_DIR: &str = "radargrams";

/// Total size of every regular file under `root`, in bytes.
///
/// Iterative and `std`-only. `walkdir` would be shorter and is already a
/// dependency, but only under the `server` feature, and a project's size is
/// not a server-shaped question -- a CLI-only build should be able to ask
/// it.
///
/// Symlinks are not followed and their targets are not counted: a link into
/// somebody else's archive is not space this project is using, and
/// following one could count the same bytes twice or walk forever.
/// Unreadable entries are skipped rather than failing the measurement --
/// a file Ridal cannot stat is not one it is about to grow, and refusing
/// every upload over one bad permission is a worse answer than a slightly
/// low total.
fn directory_size(root: &Path) -> u64 {
    let mut total = 0u64;
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            // `symlink_metadata`, so a link is measured as the link.
            let Ok(meta) = entry.path().symlink_metadata() else {
                continue;
            };
            if meta.is_dir() {
                pending.push(entry.path());
            } else {
                // Everything that is not a directory, which includes
                // symlinks. `symlink_metadata` reports a link as a link,
                // and `is_file()` is false for one -- so the earlier
                // version skipped links entirely while its comment claimed
                // they were counted. `len()` here is the link itself, a
                // handful of bytes, which is exactly the space the project
                // is using for it.
                total += meta.len();
            }
        }
    }
    total
}

/// Contents of `ridal.toml`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProjectConfig {
    #[serde(default)]
    pub project: ProjectSection,
    #[serde(default)]
    pub radargrams: RadargramsSection,
    #[serde(default)]
    pub cache: CacheSection,
    #[serde(default)]
    pub render: RenderSection,
    #[serde(default)]
    pub export: ExportSection,
    /// Which basemap the GUI's maps draw on (#177). An array of tables --
    /// `[[basemaps]]` -- rather than a section, since a project offers a
    /// list and lets each reader pick from it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub basemaps: Vec<basemaps::Basemap>,
    #[serde(default)]
    pub map: basemaps::MapSection,
    /// Vector overlays every map can draw, off by default (#177).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub overlays: Vec<overlays::Overlay>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProjectSection {
    /// Human-facing project name. Cosmetic; no identity semantics.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RadargramsSection {
    /// Directories scanned for processed radargrams. Relative paths resolve
    /// against the project root; absolute paths are used as given, so a
    /// project can index an archive it does not contain.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub roots: Vec<String>,
    /// The most the project may grow to, in bytes (#147). Unset means
    /// [`DEFAULT_MAX_PROJECT_BYTES`].
    ///
    /// A cap on the *project*, deliberately, rather than a check against
    /// free space on the host. Ridal does not own the host's disk, and
    /// asking it about free space would mean a `statvfs` dependency,
    /// platform-specific code, and a check-then-write race that can never
    /// be closed -- something else can fill the disk between the two. The
    /// project's own size is a quantity Ridal alone changes, so checking it
    /// under the store lock is a guarantee rather than a hope.
    ///
    /// An admin who owns the host can set this below what the disk has. If
    /// the disk fills anyway the write fails, the temporary file is cleaned
    /// up, and nothing is installed, because nothing is installed until the
    /// rename.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_bytes: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RenderSection {
    /// Render profile used when a request does not name one.
    ///
    /// Kept as a plain string: the set of valid profiles is a server
    /// concept, and a CLI-only build has no way to check it. Validation
    /// belongs at the HTTP boundary where a bad value can be refused with a
    /// useful message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_profile: Option<String>,

    /// Horizontal stretch applied when a radargram is opened, as a
    /// multiplier. Unset means 1x, which is the neutral value rather than a
    /// preference -- a project that never chose one stays unset in the file.
    ///
    /// Plain `f64` for the same reason `default_profile` is a plain string:
    /// which factors the viewer offers is a server concept, so the value is
    /// checked at the HTTP boundary where a bad one can be refused clearly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_xscale: Option<f64>,
}

/// Defaults for the layer-point downloads (#166).
///
/// A project section rather than only a personal one because these are
/// conventions a survey agrees on -- everyone exporting at 10 m in the same
/// CRS is what makes two people's files comparable -- while still being
/// overridable per person and per download, which is the order the settings
/// cascade already establishes.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ExportSection {
    /// Point spacing the download dialogs open on: `auto`, a distance in
    /// metres, `per-trace` or `vertices`. Unset means `auto`.
    ///
    /// A plain string for the same reason the render profile is one: which
    /// spacings the dialogs offer is a server concept, checked at the HTTP
    /// boundary where a bad value can be refused with a useful message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_spacing: Option<String>,
    /// File format the download dialogs open on, which is also the choice
    /// of coordinates: `geojson` (WGS84), `geojson-native` or `csv`. Unset
    /// means `geojson`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_format: Option<String>,
}

/// The `[render]` defaults a project can carry, as one value.
///
/// `None` on a field clears that key rather than leaving it alone: the
/// settings page always sends both, so "not set" and "unchanged" never have
/// to be told apart. A key that is absent from the file is how a project
/// says it never chose, which is different from storing the built-in value.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RenderDefaults {
    pub profile: Option<String>,
    pub xscale: Option<f64>,
}

/// The `[export]` defaults a project can carry, as one value (#166).
///
/// `None` on a field clears that key, on the same terms as
/// [`RenderDefaults`]: the form that edits these always sends both.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ExportDefaults {
    pub spacing: Option<String>,
    pub format: Option<String>,
}

/// Write `key` under `[table]`, creating the table, or remove it when the
/// value is `None`. Keeps `toml_edit`'s formatting-preserving edit in one
/// place now that there are several keys, in two tables, to apply it to.
#[cfg_attr(not(feature = "server"), allow(dead_code))]
fn set_or_clear(
    document: &mut toml_edit::DocumentMut,
    table: &str,
    key: &str,
    value: Option<toml_edit::Item>,
) {
    match value {
        Some(item) => {
            if !document.contains_key(table) {
                document[table] = toml_edit::Item::Table(toml_edit::Table::new());
            }
            document[table][key] = item;
        }
        None => {
            if let Some(existing) = document
                .get_mut(table)
                .and_then(toml_edit::Item::as_table_mut)
            {
                existing.remove(key);
            }
        }
    }
}

/// Replace an array of tables -- `[[basemaps]]`, `[[overlays]]` -- with
/// `entries`, or remove it when there are none.
///
/// Serialised through `toml` and re-parsed rather than assembled table by
/// table: the entries are plain serde types, and hand-building
/// `toml_edit::Table`s would mean a second, divergent description of the
/// same struct -- including whatever `extra` carried over from a future
/// version.
#[cfg_attr(not(feature = "server"), allow(dead_code))]
fn set_table_array<T: Serialize>(
    document: &mut toml_edit::DocumentMut,
    key: &str,
    entries: &[T],
) -> Result<(), ProjectError> {
    // Snapshotted before the array is replaced: this is the layout the file
    // already had, and the one to write it back in.
    let mut layout = top_level_layout(document);

    if entries.is_empty() {
        document.remove(key);
        layout.retain(|existing| existing != key);
        reposition(document, &layout);
        return Ok(());
    }

    // A one-key map holding the slice, so the fragment parses as `[[key]]`
    // rather than as a bare array of dicts.
    //
    // The entries are serialised straight from their own `Serialize` rather
    // than converted to a `toml::Value` first: a `Value` table is sorted,
    // which would write every record's keys alphabetically --
    // `description_field` above `id` -- and this file is read by people.
    let wrapper: std::collections::BTreeMap<&str, &[T]> = std::iter::once((key, entries)).collect();
    let text = toml::to_string(&wrapper).map_err(|e| ProjectError::Config {
        path: PathBuf::from(MARKER),
        message: format!("the {key} could not be written as TOML: {e}"),
    })?;
    let rendered: toml_edit::DocumentMut =
        text.parse()
            .map_err(|e: toml_edit::TomlError| ProjectError::Config {
                path: PathBuf::from(MARKER),
                message: format!("the {key} could not be written as TOML: {e}"),
            })?;
    let mut array = rendered
        .get(key)
        .and_then(toml_edit::Item::as_array_of_tables)
        .cloned()
        .ok_or_else(|| ProjectError::Config {
            path: PathBuf::from(MARKER),
            message: format!("the {key} did not serialise as [[{key}]]"),
        })?;
    // A serialised fragment starts flush against whatever precedes it, which
    // would glue the first `[[key]]` onto the line above.
    if let Some(first) = array.iter_mut().next() {
        first.decor_mut().set_prefix("\n");
    }
    document[key] = toml_edit::Item::ArrayOfTables(array);
    if !layout.iter().any(|existing| existing == key) {
        // New to this file: written at the end, below the commented example
        // `ridal project init` leaves rather than above it. Anywhere else
        // and the real entries would separate that note from the table it
        // describes -- the comments belong to whatever table follows them.
        layout.push(key.to_string());
    }
    reposition(document, &layout);
    Ok(())
}

/// The document's top-level tables, in the order they are written.
fn top_level_layout(document: &toml_edit::DocumentMut) -> Vec<String> {
    /// Where an item currently sits, if it is written as its own table(s).
    fn position_of(item: &toml_edit::Item) -> Option<usize> {
        match item {
            toml_edit::Item::Table(table) => table.position(),
            toml_edit::Item::ArrayOfTables(array) => {
                array.iter().filter_map(toml_edit::Table::position).min()
            }
            _ => None,
        }
    }

    let mut order: Vec<(usize, String)> = document
        .as_table()
        .iter()
        .filter_map(|(key, item)| position_of(item).map(|at| (at, key.to_string())))
        .collect();
    order.sort_by_key(|(at, _)| *at);
    order.into_iter().map(|(_, key)| key).collect()
}

/// Renumber the document's top-level tables to `layout`, writing an array of
/// tables as one contiguous block.
///
/// `toml_edit` renders top-level tables in *position* order, and the tables
/// that come out of a freshly serialised fragment carry positions 0, 1, 2 of
/// their own. Assigning such an array into a document therefore interleaves
/// its entries with whatever the file already had: the first basemap before
/// `[radargrams]`, the second between `[radargrams]` and `[render]`, and so
/// on. That parses back correctly -- TOML does not care where an array's
/// entries appear -- but `ridal.toml` is a file people are invited to edit,
/// and a settings page that shreds it across the document is not a
/// reasonable thing to do to them.
///
/// Positions are reassigned rather than nudged, so the result does not
/// depend on which numbers the fragment happened to bring with it.
fn reposition(document: &mut toml_edit::DocumentMut, layout: &[String]) {
    let mut next = 0;
    for key in layout {
        match document.get_mut(key) {
            Some(toml_edit::Item::Table(table)) => {
                table.set_position(next);
                next += 1;
            }
            Some(toml_edit::Item::ArrayOfTables(array)) => {
                for table in array.iter_mut() {
                    table.set_position(next);
                    next += 1;
                }
            }
            _ => {}
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CacheSection {
    /// Where derived data lives. Relative to the project root unless
    /// absolute -- an absolute path is the point, for a daemon whose project
    /// is on a network share but whose cache should be on local disk.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<String>,
}

/// Quote a value as a TOML basic string.
///
/// Project names are free text and paths can contain backslashes, so the
/// template cannot just wrap them in quotes and hope.
fn toml_string(value: &str) -> String {
    let escaped = value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t");
    format!("\"{escaped}\"")
}

/// An opened project.
///
/// The config sits behind a lock because the settings page edits it through
/// a shared `&AppState`, and a change that only reached the file would not
/// take effect until a restart -- which is not what pressing Save looks
/// like it does.
#[derive(Debug)]
pub struct Project {
    root: PathBuf,
    config: std::sync::RwLock<ProjectConfig>,
    documents: DocumentStore,
}

#[derive(Debug)]
pub enum ProjectError {
    NotAProject(PathBuf),
    AlreadyAProject(PathBuf),
    Io { path: PathBuf, message: String },
    Config { path: PathBuf, message: String },
}

impl std::fmt::Display for ProjectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProjectError::NotAProject(path) => write!(
                f,
                "{} is not a Ridal project (no {MARKER}). Run `ridal project init` \
                 there to create one.",
                path.display()
            ),
            ProjectError::AlreadyAProject(path) => write!(
                f,
                "{} is already a Ridal project ({MARKER} exists)",
                path.display()
            ),
            ProjectError::Io { path, message } => write!(f, "{}: {message}", path.display()),
            ProjectError::Config { path, message } => {
                write!(f, "could not read {}: {message}", path.display())
            }
        }
    }
}

impl std::error::Error for ProjectError {}

impl Project {
    /// Open the project rooted exactly at `root`.
    pub fn open(root: &Path) -> Result<Project, ProjectError> {
        let marker = root.join(MARKER);
        if !marker.is_file() {
            return Err(ProjectError::NotAProject(root.to_path_buf()));
        }
        let text = std::fs::read_to_string(&marker).map_err(|e| ProjectError::Io {
            path: marker.clone(),
            message: e.to_string(),
        })?;
        let config: ProjectConfig = toml::from_str(&text).map_err(|e| ProjectError::Config {
            path: marker,
            message: e.to_string(),
        })?;

        let root = root.to_path_buf();
        Ok(Project {
            documents: DocumentStore::new(root.clone()),
            root,
            config: std::sync::RwLock::new(config),
        })
    }

    /// Find the project containing `start`, searching upwards.
    ///
    /// Upwards rather than exact-match so that pointing Ridal at a
    /// subdirectory -- or at a single `.nc` file inside a project -- still
    /// finds the interpretations that belong to it. Returns `Ok(None)` when
    /// there is no project above `start`, which is the ordinary read-only
    /// case rather than an error.
    pub fn discover(start: &Path) -> Result<Option<Project>, ProjectError> {
        let start = std::fs::canonicalize(start).map_err(|e| ProjectError::Io {
            path: start.to_path_buf(),
            message: e.to_string(),
        })?;
        let mut current: Option<&Path> = if start.is_file() {
            start.parent()
        } else {
            Some(start.as_path())
        };
        while let Some(dir) = current {
            if dir.join(MARKER).is_file() {
                return Ok(Some(Project::open(dir)?));
            }
            current = dir.parent();
        }
        Ok(None)
    }

    /// Create a project at `root`, which need not exist yet.
    pub fn init(root: &Path, name: Option<&str>) -> Result<Project, ProjectError> {
        if root.join(MARKER).exists() {
            return Err(ProjectError::AlreadyAProject(root.to_path_buf()));
        }
        for dir in [
            root.to_path_buf(),
            root.join(INTERPRETATIONS_DIR),
            root.join(LAYERS_DIR),
            root.join(DEFAULT_RADARGRAM_DIR),
        ] {
            std::fs::create_dir_all(&dir).map_err(|e| ProjectError::Io {
                path: dir,
                message: e.to_string(),
            })?;
        }

        let config = ProjectConfig {
            project: ProjectSection {
                name: name.map(str::to_string),
            },
            radargrams: RadargramsSection {
                roots: vec![DEFAULT_RADARGRAM_DIR.to_string()],
                // Left unset so a project that never chose one follows the
                // default, and changing the default reaches it.
                max_bytes: None,
            },
            cache: CacheSection::default(),
            render: RenderSection::default(),
            export: ExportSection::default(),
            // No basemaps of its own: a new project draws on the built-in
            // one, and adding to that is a deliberate act.
            basemaps: Vec::new(),
            map: basemaps::MapSection::default(),
            overlays: Vec::new(),
        };
        // Written as a commented template rather than serialized, because
        // this file exists to be hand-edited: serde would emit a bare,
        // undocumented `[cache]` with no keys, which reads as debris rather
        // than as an invitation. `Project::open` parses either form.
        let name_line = match &config.project.name {
            Some(name) => format!("name = {}\n", toml_string(name)),
            None => format!("# name = {}\n", toml_string("My survey")),
        };
        let text = format!(
            "# Ridal project. Its presence is what makes this directory a project;\n\
             # `ridal gui .` here can then save interpretations.\n\
             \n\
             [project]\n\
             {name_line}\n\
             # Directories scanned for processed radargrams. Relative paths resolve\n\
             # against this file; absolute paths let a project index an archive it\n\
             # does not contain.\n\
             [radargrams]\n\
             roots = [{}]\n\
             \n\
             # Where derived data (rendered images, and anything else Ridal can\n\
             # rebuild) is kept. Safe to delete at any time. Point this at local\n\
             # disk if the project itself lives on a network share.\n\
             # [cache]\n\
             # dir = {}\n\
             \n\
             # Defaults applied when a page does not ask for something else.\n\
             # Set them from Project settings in the browser, or add lines\n\
             # here such as `default_profile = {}` and\n\
             # `default_xscale = 2.0`. Left unset, Ridal uses its built-in\n\
             # \"default\" profile and no horizontal stretch.\n\
             #\n\
             # A real (empty) table rather than a commented one, so that\n\
             # saving from the browser puts the key under this note instead\n\
             # of appending a second [render] elsewhere in the file.\n\
             [render]\n\
             \n\
             # Basemaps the GUI's maps can be drawn on, in addition to Ridal's\n\
             # built-in ESRI World Imagery. Add them here or from Project\n\
             # settings in the browser -- but note that saving there rewrites\n\
             # this whole block, so comments inside it are not kept.\n\
             #\n\
             # [[basemaps]]\n\
             # id = {}\n\
             # name = {}\n\
             # url = {}\n\
             # attribution = {}\n\
             # attribution_url = {}\n\
             # max_zoom = 19\n\
             \n\
             # What the layer-point download dialogs open on, so a survey\n\
             # that always exports the same way does not choose it every\n\
             # time. Set them from Project settings in the browser, or add\n\
             # lines such as `default_spacing = {}` and\n\
             # `default_format = {}`. Left unset, Ridal opens on automatic\n\
             # spacing and GeoJSON in WGS84. A person can override both in\n\
             # their own settings, and either can still be changed in the\n\
             # dialog itself.\n\
             [export]\n\
             \n\
             # Vector overlays every map can draw, off until switched on in\n\
             # its layer control. `name_field` and `description_field` name\n\
             # the feature properties the popup shows; the description is\n\
             # treated as HTML. The GeoJSON must be in WGS84, and the host\n\
             # serving it must allow this site to fetch it (CORS).\n\
             #\n\
             # [[overlays]]\n\
             # id = {}\n\
             # name = {}\n\
             # url = {}\n\
             # name_field = {}\n\
             # description_field = {}\n\
             \n\
             # Which basemap someone who has not chosen one is shown, and\n\
             # whether the built-in is offered at all. Add lines below such as\n\
             # `default_basemap = {}` or `built_in_basemap = false`. Unset\n\
             # means the first in the list, which is the built-in.\n\
             #\n\
             # The examples are in this note rather than under the header\n\
             # because a saved basemap list is appended below, and commented\n\
             # keys left inside the table would end up beneath it.\n\
             [map]\n",
            toml_string(DEFAULT_RADARGRAM_DIR),
            toml_string("/var/cache/ridal"),
            toml_string("default"),
            toml_string("10"),
            toml_string("geojson-native"),
            toml_string("osm"),
            toml_string("OpenStreetMap"),
            toml_string("https://tile.openstreetmap.org/{z}/{x}/{y}.png"),
            toml_string("© OpenStreetMap contributors"),
            toml_string("https://www.openstreetmap.org/copyright"),
            toml_string("stakes"),
            toml_string("Mass balance stakes"),
            toml_string("https://example.org/shapes/stakes.geojson"),
            toml_string("Stake"),
            toml_string("notes"),
            toml_string("osm"),
        );
        let marker = root.join(MARKER);
        std::fs::write(&marker, text).map_err(|e| ProjectError::Io {
            path: marker,
            message: e.to_string(),
        })?;

        let project = Project::open(root)?;
        // Created and tagged up front so the layout is complete from the
        // start, and so a project directory is safe to back up wholesale
        // before anything has been rendered into it.
        project.ensure_cache_dir()?;
        Ok(project)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// A snapshot of the current settings.
    ///
    /// Cloned rather than borrowed: the config is behind a lock, and
    /// handing out a guard would make every caller hold it for as long as
    /// they held the value. It is a handful of short strings.
    pub fn config(&self) -> ProjectConfig {
        self.read_config().clone()
    }

    fn read_config(&self) -> std::sync::RwLockReadGuard<'_, ProjectConfig> {
        // A poisoned lock means a panic while writing settings. The stored
        // value is still whatever was last read from disk, which is a
        // better answer than propagating the panic to every page.
        self.config
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The render profile to use when a request does not name one.
    pub fn default_profile(&self) -> Option<String> {
        self.read_config().render.default_profile.clone()
    }

    /// The horizontal stretch to open radargrams at. `None` means 1x.
    ///
    /// Only the viewer reads it, so a CLI-only build has no caller -- the
    /// same situation as `set_render_defaults` below.
    #[cfg_attr(not(feature = "server"), allow(dead_code))]
    pub fn default_xscale(&self) -> Option<f64> {
        self.read_config().render.default_xscale
    }

    /// Set (or clear) every project default the settings page edits, in the
    /// file and in memory.
    ///
    /// All four keys in one call, and therefore in one version-conditional
    /// write. Two calls would be two edits: a failure between them leaves
    /// the file holding half of what the form submitted, and two saves
    /// arriving together can interleave their halves. The form sends all
    /// four every time, so there is no caller that wants one half alone.
    ///
    /// Reached through the settings page, so a CLI-only build never calls
    /// it -- same situation as the write half of the stores beside this.
    /// [`Project::edit_marker`] holds the comment-preserving, conditional
    /// write every settings section goes through.
    #[cfg_attr(not(feature = "server"), allow(dead_code))]
    pub fn set_defaults(
        &self,
        render: &RenderDefaults,
        export: &ExportDefaults,
    ) -> Result<(), ProjectError> {
        self.edit_marker(|document| {
            set_or_clear(
                document,
                "render",
                "default_profile",
                render.profile.as_deref().map(toml_edit::value),
            );
            set_or_clear(
                document,
                "render",
                "default_xscale",
                render.xscale.map(toml_edit::value),
            );
            set_or_clear(
                document,
                "export",
                "default_spacing",
                export.spacing.as_deref().map(toml_edit::value),
            );
            set_or_clear(
                document,
                "export",
                "default_format",
                export.format.as_deref().map(toml_edit::value),
            );
            Ok(())
        })
    }

    /// The point spacing the download dialogs should open on (#166).
    #[cfg_attr(not(feature = "server"), allow(dead_code))]
    pub fn default_spacing(&self) -> Option<String> {
        self.read_config().export.default_spacing.clone()
    }

    /// The file format -- and coordinates -- the download dialogs should
    /// open on (#166).
    #[cfg_attr(not(feature = "server"), allow(dead_code))]
    pub fn default_format(&self) -> Option<String> {
        self.read_config().export.default_format.clone()
    }

    /// The basemaps this project defines, exactly as the file holds them.
    ///
    /// Callers that want the list to *draw* want
    /// [`basemaps::offered`] instead, which adds the built-in and drops
    /// entries it cannot use.
    #[cfg_attr(not(feature = "server"), allow(dead_code))]
    pub fn basemaps(&self) -> Vec<basemaps::Basemap> {
        self.read_config().basemaps.clone()
    }

    /// The `[map]` table: the default basemap, and whether the built-in is
    /// offered.
    #[cfg_attr(not(feature = "server"), allow(dead_code))]
    pub fn map_section(&self) -> basemaps::MapSection {
        self.read_config().map.clone()
    }

    /// Replace the project's basemaps and the `[map]` table that selects
    /// among them.
    ///
    /// Taken together for the same reason the render defaults are: the
    /// settings page saves them in one go, and a default naming a basemap
    /// that a half-applied change had not written yet would be a state the
    /// file should never hold.
    ///
    /// The `[[basemaps]]` block is replaced wholesale rather than patched
    /// entry by entry, so comments written *inside* it do not survive a save
    /// from the browser. Comments everywhere else in the file do, which is
    /// the trade `set_render_defaults` also makes -- and the settings page
    /// says so where the button is.
    #[cfg_attr(not(feature = "server"), allow(dead_code))]
    pub fn set_basemaps(
        &self,
        entries: &[basemaps::Basemap],
        map: &basemaps::MapSection,
    ) -> Result<(), ProjectError> {
        self.edit_marker(|document| {
            set_table_array(document, "basemaps", entries)?;
            set_or_clear(
                document,
                "map",
                "default_basemap",
                map.default_basemap.as_deref().map(toml_edit::value),
            );
            set_or_clear(
                document,
                "map",
                "built_in_basemap",
                // Absent means "offered", so only the unusual answer is
                // stored -- the rule every other setting here follows.
                map.built_in_basemap
                    .filter(|offered| !*offered)
                    .map(toml_edit::value),
            );
            Ok(())
        })
    }

    /// The vector overlays this project defines, exactly as the file holds
    /// them (#177).
    ///
    /// Callers that want the list to *draw* want [`overlays::usable`]
    /// instead, which drops entries it cannot use.
    #[cfg_attr(not(feature = "server"), allow(dead_code))]
    pub fn overlays(&self) -> Vec<overlays::Overlay> {
        self.read_config().overlays.clone()
    }

    /// Replace the project's vector overlays.
    ///
    /// A separate call from [`Project::set_basemaps`] because they are
    /// separate lists that the settings page edits in separate sections --
    /// but the same `[[...]]` rewrite, so the same caveat holds: comments
    /// inside the `[[overlays]]` block do not survive a save from the
    /// browser, and comments elsewhere in the file do.
    #[cfg_attr(not(feature = "server"), allow(dead_code))]
    pub fn set_overlays(&self, entries: &[overlays::Overlay]) -> Result<(), ProjectError> {
        self.edit_marker(|document| set_table_array(document, "overlays", entries))
    }

    /// Apply `edit` to `ridal.toml`, in the file and in memory.
    ///
    /// Edited with `toml_edit` rather than re-serialised, so the comments
    /// `ridal project init` writes survive. The file is meant to be
    /// hand-editable; a settings page that silently stripped a user's notes
    /// out of it would be a poor trade for one dropdown.
    #[cfg_attr(not(feature = "server"), allow(dead_code))]
    fn edit_marker(
        &self,
        edit: impl FnOnce(&mut toml_edit::DocumentMut) -> Result<(), ProjectError>,
    ) -> Result<(), ProjectError> {
        let marker = self.root.join(MARKER);
        // Read through the store so the version comes with the text. The
        // write below is conditional on it: the store's lock serialises the
        // writes but not the read-modify-write around them, so two saves
        // arriving together would otherwise both read the old file and the
        // second would discard the first's key.
        let current = self
            .documents
            .read(Path::new(MARKER))
            .map_err(|e| ProjectError::Io {
                path: marker.clone(),
                message: e.to_string(),
            })?
            .ok_or_else(|| ProjectError::Io {
                path: marker.clone(),
                message: "the project marker has gone missing".to_string(),
            })?;
        let mut document: toml_edit::DocumentMut =
            current
                .text
                .parse()
                .map_err(|e: toml_edit::TomlError| ProjectError::Config {
                    path: marker.clone(),
                    message: e.to_string(),
                })?;

        edit(&mut document)?;

        let updated = document.to_string();
        // Atomic, and conditional on the version just read, so a save that
        // raced another one is refused rather than silently winning.
        self.documents
            .write(
                Path::new(MARKER),
                &updated,
                &store::Expectation::Version(current.version),
            )
            .map_err(|e| ProjectError::Io {
                path: marker.clone(),
                message: e.to_string(),
            })?;

        // Re-parsed from what was written rather than patched in memory, so
        // the two cannot drift.
        let reparsed: ProjectConfig =
            toml::from_str(&updated).map_err(|e| ProjectError::Config {
                path: marker,
                message: e.to_string(),
            })?;
        let mut guard = self
            .config
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *guard = reparsed;
        Ok(())
    }

    /// The store holding authored documents.
    pub fn documents(&self) -> &DocumentStore {
        &self.documents
    }

    /// Absolute directories to scan for processed radargrams.
    ///
    /// Falls back to the project root itself when the config lists none, so
    /// a project whose `.nc` files sit loose at the top level still works
    /// without configuration.
    pub fn radargram_roots(&self) -> Vec<PathBuf> {
        let config = self.read_config();
        if config.radargrams.roots.is_empty() {
            return vec![self.root.clone()];
        }
        config
            .radargrams
            .roots
            .iter()
            .map(|entry| self.resolve(entry))
            .collect()
    }

    /// How large this project may grow, in bytes.
    pub fn max_bytes(&self) -> u64 {
        self.read_config()
            .radargrams
            .max_bytes
            .unwrap_or(DEFAULT_MAX_PROJECT_BYTES)
    }

    /// How large the project is now, by walking it.
    ///
    /// Walked rather than maintained as a running total, which cannot
    /// drift: a total is wrong the moment anything writes to the project
    /// without going through the counter, and it is wrong silently. A few
    /// hundred `stat` calls on an upload is not a cost worth being clever
    /// about.
    ///
    /// Unreadable entries are skipped rather than failing the whole
    /// measurement. A file Ridal cannot stat is not one it is about to
    /// grow, and refusing every upload because of one bad permission would
    /// be a worse answer than a slightly low total.
    pub fn size_bytes(&self) -> u64 {
        directory_size(&self.root)
    }

    /// Where derived data belongs.
    ///
    /// Reserved now and created on demand: the on-disk render cache does not
    /// exist yet, but its location is a project-shaped decision and settling
    /// it here means adding the cache later is not also a layout change.
    pub fn cache_dir(&self) -> PathBuf {
        match &self.read_config().cache.dir {
            Some(dir) => self.resolve(dir),
            None => self.root.join(DEFAULT_CACHE_DIR),
        }
    }

    /// Create the cache directory and mark it as derived data.
    ///
    /// `CACHEDIR.TAG` is the convention backup and archiving tools already
    /// recognise (Cargo tags `target/` the same way), so a project directory
    /// can be backed up wholesale without dragging along regenerable
    /// renders.
    pub fn ensure_cache_dir(&self) -> Result<PathBuf, ProjectError> {
        let dir = self.cache_dir();
        std::fs::create_dir_all(&dir).map_err(|e| ProjectError::Io {
            path: dir.clone(),
            message: e.to_string(),
        })?;
        let tag = dir.join("CACHEDIR.TAG");
        if !tag.exists() {
            let contents = "Signature: 8a477f597d28d172789f06886806bc55\n\
                            # This file is a cache directory tag created by ridal.\n\
                            # For information about cache directory tags, see:\n\
                            #\thttps://bford.info/cachedir/\n";
            std::fs::write(&tag, contents).map_err(|e| ProjectError::Io {
                path: tag,
                message: e.to_string(),
            })?;
        }
        Ok(dir)
    }

    fn resolve(&self, entry: &str) -> PathBuf {
        let path = Path::new(entry);
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.root.join(path)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_creates_a_marker_and_the_store_directories() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("myproject");
        let project = Project::init(&root, Some("Drønbreen 2022")).unwrap();

        assert!(root.join(MARKER).is_file());
        assert!(root.join(INTERPRETATIONS_DIR).is_dir());
        assert!(root.join(LAYERS_DIR).is_dir());
        assert_eq!(
            project.config().project.name.as_deref(),
            Some("Drønbreen 2022")
        );
    }

    #[test]
    fn init_refuses_to_overwrite_an_existing_project() {
        let dir = tempfile::tempdir().unwrap();
        Project::init(dir.path(), None).unwrap();
        assert!(matches!(
            Project::init(dir.path(), None),
            Err(ProjectError::AlreadyAProject(_))
        ));
    }

    #[test]
    fn a_directory_without_a_marker_is_not_a_project() {
        // The existing read-only behaviour depends on this: pointing Ridal
        // at a bare directory of .nc files must not turn it into a project.
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            Project::open(dir.path()),
            Err(ProjectError::NotAProject(_))
        ));
        assert!(Project::discover(dir.path()).unwrap().is_none());
    }

    #[test]
    fn discover_walks_up_from_a_subdirectory() {
        let dir = tempfile::tempdir().unwrap();
        Project::init(dir.path(), None).unwrap();
        let nested = dir.path().join("radargrams").join("2022").join("deep");
        std::fs::create_dir_all(&nested).unwrap();

        let found = Project::discover(&nested).unwrap().unwrap();
        assert_eq!(
            std::fs::canonicalize(found.root()).unwrap(),
            std::fs::canonicalize(dir.path()).unwrap()
        );
    }

    #[test]
    fn discover_walks_up_from_a_file_inside_the_project() {
        let dir = tempfile::tempdir().unwrap();
        Project::init(dir.path(), None).unwrap();
        let file = dir.path().join("radargrams").join("line.nc");
        std::fs::write(&file, b"not really a netcdf").unwrap();

        assert!(Project::discover(&file).unwrap().is_some());
    }

    #[test]
    fn radargram_roots_resolve_relative_and_absolute_entries() {
        let dir = tempfile::tempdir().unwrap();
        let project = Project::init(dir.path(), None).unwrap();
        assert_eq!(
            project.radargram_roots(),
            vec![dir.path().join(DEFAULT_RADARGRAM_DIR)]
        );

        // An absolute root is how a project indexes an archive it does not
        // contain. Built from a real temporary directory rather than a
        // literal like "/mnt/archive": that is not absolute on Windows, so
        // `resolve` would correctly join it to the project root and the
        // assertion would fail for the wrong reason.
        let elsewhere = tempfile::tempdir().unwrap();
        let absolute = elsewhere.path().to_str().unwrap();
        let text = format!(
            "[radargrams]\nroots = [\"inside\", {}]\n",
            toml_string(absolute)
        );
        std::fs::write(dir.path().join(MARKER), text).unwrap();
        let project = Project::open(dir.path()).unwrap();
        assert_eq!(
            project.radargram_roots(),
            vec![dir.path().join("inside"), PathBuf::from(absolute)]
        );
    }

    #[test]
    fn project_size_counts_files_at_every_depth() {
        let dir = tempfile::tempdir().unwrap();
        let project = Project::init(dir.path(), None).unwrap();
        let before = project.size_bytes();

        std::fs::create_dir_all(dir.path().join("radargrams/deep")).unwrap();
        std::fs::write(dir.path().join("radargrams/a.nc"), vec![0u8; 1000]).unwrap();
        std::fs::write(dir.path().join("radargrams/deep/b.nc"), vec![0u8; 2000]).unwrap();

        assert_eq!(project.size_bytes(), before + 3000);
    }

    #[test]
    fn a_symlinked_archive_is_not_counted_as_the_projects_own_space() {
        // A link into somebody else's archive is not space this project is
        // using, and following one could count the same bytes twice or walk
        // forever.
        #[cfg(unix)]
        {
            let dir = tempfile::tempdir().unwrap();
            let elsewhere = tempfile::tempdir().unwrap();
            let project = Project::init(dir.path(), None).unwrap();
            std::fs::write(elsewhere.path().join("big.nc"), vec![0u8; 100_000]).unwrap();
            let before = project.size_bytes();

            std::os::unix::fs::symlink(elsewhere.path(), dir.path().join("archive")).unwrap();

            // The link counts as a link -- a few dozen bytes, which is
            // what the project is actually using for it. Its 100 kB target
            // does not, because that space belongs to whoever owns it.
            let after = project.size_bytes();
            assert!(after > before, "the link itself is a file");
            assert!(
                after - before < 1000,
                "the target must not be counted: grew by {}",
                after - before
            );
        }
    }

    #[test]
    fn the_size_cap_defaults_until_a_project_sets_one() {
        // Unset rather than written at init, so changing the default
        // reaches every project that never chose.
        let dir = tempfile::tempdir().unwrap();
        let project = Project::init(dir.path(), None).unwrap();
        assert_eq!(project.max_bytes(), DEFAULT_MAX_PROJECT_BYTES);
        assert!(!std::fs::read_to_string(dir.path().join(MARKER))
            .unwrap()
            .contains("max_bytes"));
    }

    #[test]
    fn radargram_roots_fall_back_to_the_project_root() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(MARKER), "[project]\nname = \"x\"\n").unwrap();
        let project = Project::open(dir.path()).unwrap();
        assert_eq!(project.radargram_roots(), vec![dir.path().to_path_buf()]);
    }

    #[test]
    fn the_cache_directory_can_be_moved_off_the_project() {
        // The daemon case: project on a network share, cache on local disk.
        //
        // A real temporary directory rather than a literal "/var/cache":
        // that is not an absolute path on Windows, where `resolve` would
        // rightly treat it as relative to the project.
        let dir = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let absolute = cache.path().to_str().unwrap();
        std::fs::write(
            dir.path().join(MARKER),
            format!("[cache]\ndir = {}\n", toml_string(absolute)),
        )
        .unwrap();
        let project = Project::open(dir.path()).unwrap();
        assert_eq!(project.cache_dir(), PathBuf::from(absolute));
    }

    #[test]
    fn the_cache_directory_is_tagged_as_derived_data() {
        let dir = tempfile::tempdir().unwrap();
        let project = Project::init(dir.path(), None).unwrap();
        let cache = project.ensure_cache_dir().unwrap();

        let tag = std::fs::read_to_string(cache.join("CACHEDIR.TAG")).unwrap();
        assert!(
            tag.starts_with("Signature: 8a477f597d28d172789f06886806bc55"),
            "the signature line is what backup tools actually match on"
        );
        // Idempotent: opening a project twice must not fail on the tag.
        project.ensure_cache_dir().unwrap();
    }

    #[test]
    fn the_default_profile_round_trips_and_keeps_the_comments() {
        let dir = tempfile::tempdir().unwrap();
        let project = Project::init(dir.path(), Some("x")).unwrap();
        assert_eq!(project.default_profile(), None);

        let before = std::fs::read_to_string(dir.path().join(MARKER)).unwrap();
        let comment_lines = before.lines().filter(|l| l.starts_with('#')).count();
        assert!(comment_lines > 5, "the template should be commented");

        set_profile(&project, Some("abslog"));

        // In memory straight away -- a save that only reached the file
        // would not take effect until a restart.
        assert_eq!(project.default_profile().as_deref(), Some("abslog"));
        // And on disk, for the next process.
        assert_eq!(
            Project::open(dir.path())
                .unwrap()
                .default_profile()
                .as_deref(),
            Some("abslog")
        );

        let after = std::fs::read_to_string(dir.path().join(MARKER)).unwrap();
        assert_eq!(
            after.lines().filter(|l| l.starts_with('#')).count(),
            comment_lines,
            "editing the file must not strip the comments in it:\n{after}"
        );
        // The other settings are untouched.
        assert!(after.contains("[radargrams]"), "{after}");
    }

    /// Set only the profile, leaving the horizontal scale cleared. Most of
    /// these tests predate there being a second key and only care about one.
    fn set_profile(project: &Project, profile: Option<&str>) {
        project
            .set_defaults(
                &RenderDefaults {
                    profile: profile.map(str::to_string),
                    xscale: None,
                },
                &ExportDefaults::default(),
            )
            .unwrap();
    }

    fn a_basemap(id: &str) -> basemaps::Basemap {
        basemaps::Basemap {
            id: id.to_string(),
            name: format!("Basemap {id}"),
            url: format!("https://tile.example.org/{id}/{{z}}/{{x}}/{{y}}.png"),
            attribution: Some("Example".to_string()),
            attribution_url: None,
            tile_size: None,
            max_zoom: Some(19),
            zoom_offset: None,
            subdomains: None,
            extra: serde_json::Map::new(),
        }
    }

    #[test]
    fn basemaps_round_trip_through_the_file_and_keep_the_comments() {
        let dir = tempfile::tempdir().unwrap();
        let project = Project::init(dir.path(), None).unwrap();
        assert!(project.basemaps().is_empty());

        let before = std::fs::read_to_string(dir.path().join(MARKER)).unwrap();
        let comment_lines = before
            .lines()
            .filter(|l| l.trim_start().starts_with('#'))
            .count();

        project
            .set_basemaps(
                &[a_basemap("osm"), a_basemap("topo")],
                &basemaps::MapSection {
                    default_basemap: Some("topo".to_string()),
                    built_in_basemap: Some(true),
                },
            )
            .unwrap();

        // In memory straight away, and on disk for the next process.
        assert_eq!(project.basemaps().len(), 2);
        let reopened = Project::open(dir.path()).unwrap();
        assert_eq!(
            reopened.basemaps(),
            vec![a_basemap("osm"), a_basemap("topo")]
        );
        assert_eq!(
            reopened.map_section().default_basemap.as_deref(),
            Some("topo")
        );
        assert!(reopened.map_section().offers_built_in());

        let after = std::fs::read_to_string(dir.path().join(MARKER)).unwrap();
        // The comments outside the basemap block survive, which is the whole
        // reason this goes through toml_edit rather than a re-serialisation.
        assert!(
            after
                .lines()
                .filter(|l| l.trim_start().starts_with('#'))
                .count()
                >= comment_lines,
            "editing must not strip the file's comments:\n{after}"
        );
        assert!(after.contains("[radargrams]"), "{after}");
        // Unset optional keys stay out of the file, so a basemap that never
        // chose a tile size follows Ridal's default if it ever changes.
        assert!(!after.contains("tile_size"), "{after}");
    }

    #[test]
    fn saved_basemaps_are_written_as_one_block_rather_than_scattered() {
        // toml_edit writes top-level tables in position order, and a freshly
        // serialised array brings positions 0, 1, 2 of its own -- so the
        // entries landed *between* the file's existing tables: one before
        // [radargrams], the next between [radargrams] and [render]. Valid
        // TOML, unreadable file. Caught in a browser, pinned here.
        let dir = tempfile::tempdir().unwrap();
        let project = Project::init(dir.path(), None).unwrap();
        project
            .set_basemaps(
                &[a_basemap("osm"), a_basemap("topo"), a_basemap("aerial")],
                &basemaps::MapSection {
                    default_basemap: Some("osm".to_string()),
                    built_in_basemap: None,
                },
            )
            .unwrap();

        let text = std::fs::read_to_string(dir.path().join(MARKER)).unwrap();
        let headings: Vec<&str> = text
            .lines()
            .map(str::trim)
            .filter(|line| line.starts_with('[') && !line.starts_with("[["))
            .chain(text.lines().map(str::trim).filter(|l| l.starts_with("[[")))
            .collect();
        assert!(!headings.is_empty(), "{text}");

        // The three entries are consecutive, and `[map]` -- which names one
        // of them -- comes after the list rather than inside it.
        let order: Vec<&str> = text
            .lines()
            .map(str::trim)
            .filter(|line| line.starts_with('['))
            .collect();
        let first = order.iter().position(|l| *l == "[[basemaps]]").unwrap();
        assert_eq!(
            &order[first..first + 3],
            &["[[basemaps]]", "[[basemaps]]", "[[basemaps]]"],
            "the basemaps must be written together:\n{text}"
        );
        // Below the commented example the template leaves, not above it: a
        // comment block belongs to the table that follows it, so entries
        // inserted higher up would orphan the note that explains them.
        assert!(
            order.iter().position(|l| *l == "[map]").unwrap() < first,
            "the saved list goes after the tables that were already there:\n{text}"
        );
        // And it still parses back to what was saved.
        assert_eq!(Project::open(dir.path()).unwrap().basemaps().len(), 3);
    }

    #[test]
    fn overlays_round_trip_and_sit_beside_the_basemaps() {
        // Two arrays of tables in one file (#177). Each is rewritten whole
        // and must leave the other -- and the rest of the file -- alone.
        let dir = tempfile::tempdir().unwrap();
        let project = Project::init(dir.path(), None).unwrap();
        assert!(project.overlays().is_empty());

        let stakes = overlays::Overlay {
            id: "stakes".to_string(),
            name: "Mass balance stakes".to_string(),
            url: "https://static.example.org/shapes/stakes.geojson".to_string(),
            name_field: Some("Stake".to_string()),
            description_field: Some("notes".to_string()),
            color: None,
            extra: serde_json::Map::new(),
        };
        project
            .set_basemaps(&[a_basemap("osm")], &basemaps::MapSection::default())
            .unwrap();
        project.set_overlays(&[stakes.clone()]).unwrap();

        let reopened = Project::open(dir.path()).unwrap();
        assert_eq!(reopened.overlays(), vec![stakes]);
        assert_eq!(reopened.basemaps().len(), 1, "the basemaps are untouched");

        let text = std::fs::read_to_string(dir.path().join(MARKER)).unwrap();
        assert!(text.contains("[[overlays]]"), "{text}");
        assert!(text.contains("name_field = \"Stake\""), "{text}");
        // Written in the order the record declares, not alphabetically:
        // serialising through `toml::Value` sorts the keys, which put
        // `description_field` above `id` and made the file read as noise.
        // Comments are dropped first -- the template's own commented
        // example mentions the same keys in the same order this checks.
        let live: String = text
            .lines()
            .filter(|line| !line.trim_start().starts_with('#'))
            .collect::<Vec<_>>()
            .join("\n");
        let id_at = live.find("id = \"stakes\"").unwrap();
        let description_at = live.find("description_field").unwrap();
        assert!(
            id_at < description_at,
            "keys must stay in record order:\n{live}"
        );
        // Unset optionals stay out of the file, as everywhere else here.
        assert!(!text.contains("color"), "{text}");

        // And removing them takes the block with it.
        project.set_overlays(&[]).unwrap();
        let text = std::fs::read_to_string(dir.path().join(MARKER)).unwrap();
        let live = text
            .lines()
            .filter(|l| !l.trim_start().starts_with('#'))
            .filter(|l| l.contains("[[overlays]]"))
            .count();
        assert_eq!(live, 0, "{text}");
        assert_eq!(
            Project::open(dir.path()).unwrap().basemaps().len(),
            1,
            "removing the overlays must not remove the basemaps:\n{text}"
        );
    }

    #[test]
    fn the_built_in_is_only_written_when_it_is_switched_off() {
        // Absence means "offered", the rule every other setting here follows,
        // so the ordinary case leaves no key behind at all.
        let dir = tempfile::tempdir().unwrap();
        let project = Project::init(dir.path(), None).unwrap();
        project
            .set_basemaps(
                &[],
                &basemaps::MapSection {
                    default_basemap: None,
                    built_in_basemap: Some(true),
                },
            )
            .unwrap();
        let text = std::fs::read_to_string(dir.path().join(MARKER)).unwrap();
        let live = text
            .lines()
            .filter(|l| !l.trim_start().starts_with('#'))
            .filter(|l| l.contains("built_in_basemap"))
            .count();
        assert_eq!(live, 0, "{text}");

        project
            .set_basemaps(
                &[a_basemap("osm")],
                &basemaps::MapSection {
                    default_basemap: None,
                    built_in_basemap: Some(false),
                },
            )
            .unwrap();
        assert!(!Project::open(dir.path())
            .unwrap()
            .map_section()
            .offers_built_in());
    }

    #[test]
    fn removing_every_basemap_removes_the_block() {
        let dir = tempfile::tempdir().unwrap();
        let project = Project::init(dir.path(), None).unwrap();
        let section = basemaps::MapSection::default();
        project.set_basemaps(&[a_basemap("osm")], &section).unwrap();
        assert!(std::fs::read_to_string(dir.path().join(MARKER))
            .unwrap()
            .contains("[[basemaps]]"));

        project.set_basemaps(&[], &section).unwrap();

        let text = std::fs::read_to_string(dir.path().join(MARKER)).unwrap();
        let live = text
            .lines()
            .filter(|l| !l.trim_start().starts_with('#'))
            .filter(|l| l.contains("[[basemaps]]"))
            .count();
        assert_eq!(live, 0, "{text}");
        assert!(Project::open(dir.path()).unwrap().basemaps().is_empty());
    }

    #[test]
    fn saving_basemaps_leaves_the_render_defaults_alone() {
        // The two halves of the settings page write the same file. A save
        // from one that quietly cleared the other's key would be the kind of
        // bug nobody attributes to the right cause.
        let dir = tempfile::tempdir().unwrap();
        let project = Project::init(dir.path(), None).unwrap();
        project
            .set_defaults(
                &RenderDefaults {
                    profile: Some("abslog".to_string()),
                    xscale: Some(2.0),
                },
                &ExportDefaults::default(),
            )
            .unwrap();

        project
            .set_basemaps(&[a_basemap("osm")], &basemaps::MapSection::default())
            .unwrap();

        let reopened = Project::open(dir.path()).unwrap();
        assert_eq!(reopened.default_profile().as_deref(), Some("abslog"));
        assert_eq!(reopened.default_xscale(), Some(2.0));
        assert_eq!(reopened.basemaps().len(), 1);
    }

    #[test]
    fn a_hand_written_basemap_is_read_as_it_was_written() {
        // `ridal.toml` is meant to be hand-edited, and the commented example
        // `ridal project init` writes is in exactly this shape.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(MARKER),
            "[project]\n\
             name = \"x\"\n\
             \n\
             [[basemaps]]\n\
             id = \"osm\"\n\
             name = \"OpenStreetMap\"\n\
             url = \"https://tile.openstreetmap.org/{z}/{x}/{y}.png\"\n\
             attribution = \"© OpenStreetMap contributors\"\n\
             max_zoom = 19\n\
             \n\
             [map]\n\
             default_basemap = \"osm\"\n\
             built_in_basemap = false\n",
        )
        .unwrap();

        let project = Project::open(dir.path()).unwrap();
        let entries = project.basemaps();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, "osm");
        assert_eq!(entries[0].max_zoom(), 19);
        assert_eq!(entries[0].tile_size(), basemaps::DEFAULT_TILE_SIZE);
        entries[0].validate().unwrap();
        assert!(!project.map_section().offers_built_in());
        assert_eq!(
            project.map_section().default_basemap.as_deref(),
            Some("osm")
        );
    }

    #[test]
    fn a_settings_save_that_raced_another_is_refused_not_silently_applied() {
        // The store's lock serialises the writes but not the
        // read-modify-write around them. Without a conditional write, two
        // saves arriving together both read the old file and the second
        // discards the first's key rather than conflicting.
        let dir = tempfile::tempdir().unwrap();
        let project = Project::init(dir.path(), None).unwrap();
        set_profile(&project, Some("abslog"));

        // Simulate the other writer: change the file behind this project's
        // back, so the version it would have read is stale.
        let marker = dir.path().join(MARKER);
        let text = std::fs::read_to_string(&marker).unwrap();
        std::fs::write(&marker, format!("{text}\n# someone else edited this\n")).unwrap();

        // A save that read the *current* file still succeeds -- the point
        // is that the version is checked, not that saving is fragile.
        let reopened = Project::open(dir.path()).unwrap();
        reopened
            .set_defaults(
                &RenderDefaults {
                    profile: Some("positive".to_string()),
                    xscale: None,
                },
                &ExportDefaults::default(),
            )
            .unwrap();
        assert_eq!(reopened.default_profile().as_deref(), Some("positive"));
        // The other writer's line survived, because the edit was applied to
        // the text that was actually on disk.
        let after = std::fs::read_to_string(&marker).unwrap();
        assert!(after.contains("someone else edited this"), "{after}");
    }

    #[test]
    fn both_render_defaults_are_written_in_one_edit() {
        let dir = tempfile::tempdir().unwrap();
        let project = Project::init(dir.path(), None).unwrap();
        project
            .set_defaults(
                &RenderDefaults {
                    profile: Some("abslog".to_string()),
                    xscale: Some(2.0),
                },
                &ExportDefaults::default(),
            )
            .unwrap();

        assert_eq!(project.default_profile().as_deref(), Some("abslog"));
        assert_eq!(project.default_xscale(), Some(2.0));

        // And for the next process, from the file rather than memory.
        let reopened = Project::open(dir.path()).unwrap();
        assert_eq!(reopened.default_profile().as_deref(), Some("abslog"));
        assert_eq!(reopened.default_xscale(), Some(2.0));
    }

    #[test]
    fn clearing_one_render_default_leaves_the_other() {
        let dir = tempfile::tempdir().unwrap();
        let project = Project::init(dir.path(), None).unwrap();
        project
            .set_defaults(
                &RenderDefaults {
                    profile: Some("abslog".to_string()),
                    xscale: Some(4.0),
                },
                &ExportDefaults::default(),
            )
            .unwrap();
        // Back to 1x, which is stored as absence, while the profile stays.
        project
            .set_defaults(
                &RenderDefaults {
                    profile: Some("abslog".to_string()),
                    xscale: None,
                },
                &ExportDefaults::default(),
            )
            .unwrap();

        assert_eq!(project.default_xscale(), None);
        assert_eq!(project.default_profile().as_deref(), Some("abslog"));
        let text = std::fs::read_to_string(dir.path().join(MARKER)).unwrap();
        let live = text
            .lines()
            .filter(|l| !l.trim_start().starts_with('#'))
            .filter(|l| l.contains("default_xscale"))
            .count();
        assert_eq!(live, 0, "{text}");
    }

    #[test]
    fn clearing_the_default_profile_removes_the_key() {
        let dir = tempfile::tempdir().unwrap();
        let project = Project::init(dir.path(), None).unwrap();
        set_profile(&project, Some("abslog"));
        set_profile(&project, None);

        assert_eq!(project.default_profile(), None);
        let text = std::fs::read_to_string(dir.path().join(MARKER)).unwrap();
        // Only the commented example from the template should remain.
        let live = text
            .lines()
            .filter(|l| !l.trim_start().starts_with('#'))
            .filter(|l| l.contains("default_profile"))
            .count();
        assert_eq!(live, 0, "{text}");
    }

    #[test]
    fn a_settings_write_leaves_no_temporary_file_behind() {
        let dir = tempfile::tempdir().unwrap();
        let project = Project::init(dir.path(), None).unwrap();
        set_profile(&project, Some("positive"));

        let strays: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.contains(".tmp"))
            .collect();
        assert!(strays.is_empty(), "{strays:?}");
    }

    #[test]
    fn an_unparseable_marker_is_reported_rather_than_ignored() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(MARKER), "this is not toml {{{").unwrap();
        assert!(matches!(
            Project::open(dir.path()),
            Err(ProjectError::Config { .. })
        ));
    }

    #[test]
    fn unknown_config_keys_are_tolerated() {
        // Forward compatibility: a project written by a newer Ridal should
        // still open in an older one rather than refusing outright.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(MARKER),
            "[project]\nname = \"x\"\n\n[future]\nsomething = 1\n",
        )
        .unwrap();
        assert!(Project::open(dir.path()).is_ok());
    }
}
