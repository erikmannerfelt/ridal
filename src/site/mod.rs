//! A **site**: server-wide identity and hosting for one or more projects
//! (#214).
//!
//! A site is a directory holding the things that belong to the *server*
//! rather than to any one survey:
//!
//! ```text
//! my_site/
//!   ridal-site.toml      marker, name, format version, archived keys
//!   accounts.json        server-wide accounts (0600)
//!   session.key          site-wide cookie signing key (0600)
//!   projects/
//!     glac-2026/         ordinary project: ridal.toml + ridal_data/
//!     share-anna/
//! ```
//!
//! A project stays exactly what it always was, portable and self-contained.
//! Once inside a site it is addressed by an immutable [`ProjectKey`] -- the
//! directory name under `projects/` -- so renaming its display name never
//! breaks a link. Its `users.json` holds **memberships** (an account's role
//! and download scope in that project) rather than accounts and passwords;
//! those live at the site level in [`accounts`].
//!
//! # No migration
//!
//! A project whose `users.json` still holds accounts is **refused**, the way
//! [`crate::project::Project::open`] refuses a pre-#187 project. Existing
//! single-project deployments are short-lived courses, so guessing a
//! migration from them is worse than saying so plainly. [`Site::project`]
//! is where the refusal happens, because a project opened on its own by
//! `ridal gui` must keep working exactly as it did.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use serde::{Deserialize, Serialize};

use crate::identity::ProjectKey;
use crate::project::store::{DocumentStore, Expectation, StoreError, Version};
use crate::project::{Project, ProjectError};

pub mod accounts;
pub mod audit;

/// The site's marker and settings file, at its root.
pub const SITE_MARKER: &str = "ridal-site.toml";

/// The layout version this Ridal understands.
pub const SITE_FORMAT_VERSION: u32 = 1;

/// Where projects live, relative to the site root.
pub const PROJECTS_DIR: &str = "projects";

/// The project document that must not carry accounts (see module doc).
const MEMBERS_FILE: &str = "users.json";

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SiteConfig {
    #[serde(default)]
    pub site: SiteSection,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SiteSection {
    /// A human-readable site name, editable and never an identifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Written on `init`; a newer value is refused rather than guessed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format_version: Option<u32>,
    /// Project keys that have been archived: read-only, still exportable.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub archived: Vec<ProjectKey>,
}

#[derive(Debug)]
pub enum SiteError {
    NotASite(PathBuf),
    AlreadyASite(PathBuf),
    /// A site written by a newer Ridal than this one.
    UnsupportedFormat {
        path: PathBuf,
        version: u32,
    },
    Io {
        path: PathBuf,
        message: String,
    },
    Config {
        path: PathBuf,
        message: String,
    },
    Store(StoreError),
    /// A project whose `users.json` still holds accounts, not memberships.
    LegacyAccounts {
        project: PathBuf,
        path: PathBuf,
    },
    /// No project with this key exists in the site.
    NotFound(String),
    /// A project directory already exists at this key.
    KeyInUse(String),
}

impl fmt::Display for SiteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SiteError::NotASite(path) => write!(
                f,
                "{} is not a Ridal site (no {SITE_MARKER}). Run `ridal site init {}` \
                 to make one, or `ridal gui` for local single-project work.",
                path.display(),
                path.display()
            ),
            SiteError::AlreadyASite(path) => write!(
                f,
                "{} is already a Ridal site ({SITE_MARKER} exists)",
                path.display()
            ),
            SiteError::UnsupportedFormat { path, version } => write!(
                f,
                "{} says format_version = {version}, but this Ridal only knows up to \
                 {SITE_FORMAT_VERSION}. Upgrade Ridal rather than opening it with this \
                 one, which would write to the wrong places.",
                path.display()
            ),
            SiteError::Io { path, message } => write!(f, "{}: {message}", path.display()),
            SiteError::Config { path, message } => {
                write!(f, "could not read {}: {message}", path.display())
            }
            SiteError::Store(e) => write!(f, "{e}"),
            SiteError::LegacyAccounts { project, path } => write!(
                f,
                "{} holds accounts ({}), which a site does not support: accounts are \
                 server-wide now, and a project holds memberships. This Ridal does not \
                 migrate them. Recreate the people as site accounts and add them to the \
                 project as members.",
                project.display(),
                path.display()
            ),
            SiteError::NotFound(key) => {
                write!(f, "No project '{key}' in this site.")
            }
            SiteError::KeyInUse(key) => write!(
                f,
                "A project already exists at key '{key}'. Project keys are immutable; \
                 choose another."
            ),
        }
    }
}

impl std::error::Error for SiteError {}

impl From<StoreError> for SiteError {
    fn from(error: StoreError) -> Self {
        SiteError::Store(error)
    }
}

/// A site on disk. Holds no project state itself; projects are opened on
/// demand through [`Site::project`].
#[derive(Debug)]
pub struct Site {
    root: PathBuf,
    config: RwLock<SiteConfig>,
    /// Rooted at the site root, holding one document: `ridal-site.toml`.
    manifest: DocumentStore,
}

impl Site {
    /// Create a site at `root`, which need not exist yet.
    pub fn init(root: &Path, name: Option<&str>) -> Result<Site, SiteError> {
        if root.join(SITE_MARKER).exists() {
            return Err(SiteError::AlreadyASite(root.to_path_buf()));
        }
        for dir in [root.to_path_buf(), root.join(PROJECTS_DIR)] {
            std::fs::create_dir_all(&dir).map_err(|e| SiteError::Io {
                path: dir,
                message: e.to_string(),
            })?;
        }
        let config = SiteConfig {
            site: SiteSection {
                name: name.map(str::to_string),
                format_version: Some(SITE_FORMAT_VERSION),
                archived: Vec::new(),
            },
        };
        let site = Site {
            manifest: DocumentStore::new(root.to_path_buf()),
            root: root.to_path_buf(),
            config: RwLock::new(config.clone()),
        };
        site.write_config(&config, &Expectation::Absent)?;
        Ok(site)
    }

    /// Open the site rooted exactly at `root`.
    pub fn open(root: &Path) -> Result<Site, SiteError> {
        let marker = root.join(SITE_MARKER);
        if !marker.is_file() {
            return Err(SiteError::NotASite(root.to_path_buf()));
        }
        let text = std::fs::read_to_string(&marker).map_err(|e| SiteError::Io {
            path: marker.clone(),
            message: e.to_string(),
        })?;
        let config: SiteConfig = toml::from_str(&text).map_err(|e| SiteError::Config {
            path: marker.clone(),
            message: e.to_string(),
        })?;
        if let Some(version) = config.site.format_version {
            if version > SITE_FORMAT_VERSION {
                return Err(SiteError::UnsupportedFormat {
                    path: marker,
                    version,
                });
            }
        }
        Ok(Site {
            manifest: DocumentStore::new(root.to_path_buf()),
            root: root.to_path_buf(),
            config: RwLock::new(config),
        })
    }

    /// Find the site containing `start`, searching upwards.
    ///
    /// Returns `Ok(None)` when there is none above, which is the ordinary
    /// local case rather than an error.
    pub fn discover(start: &Path) -> Result<Option<Site>, SiteError> {
        let start = std::fs::canonicalize(start).map_err(|e| SiteError::Io {
            path: start.to_path_buf(),
            message: e.to_string(),
        })?;
        let mut current: Option<&Path> = if start.is_file() {
            start.parent()
        } else {
            Some(start.as_path())
        };
        while let Some(dir) = current {
            if dir.join(SITE_MARKER).is_file() {
                return Ok(Some(Site::open(dir)?));
            }
            current = dir.parent();
        }
        Ok(None)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The store rooted at the site root, holding `ridal-site.toml`,
    /// `accounts.json` and (once a session is persisted) `session.key`.
    pub fn store(&self) -> &DocumentStore {
        &self.manifest
    }

    /// The site's editable display name, falling back to the directory name.
    pub fn name(&self) -> String {
        let config = self.config.read().expect("site config lock poisoned");
        config
            .site
            .name
            .clone()
            .unwrap_or_else(|| self.directory_name())
    }

    fn directory_name(&self) -> String {
        self.root
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "site".to_string())
    }

    pub fn config(&self) -> SiteConfig {
        self.config
            .read()
            .expect("site config lock poisoned")
            .clone()
    }

    pub fn projects_dir(&self) -> PathBuf {
        self.root.join(PROJECTS_DIR)
    }

    pub fn project_path(&self, key: &ProjectKey) -> PathBuf {
        self.projects_dir().join(key.as_str())
    }

    /// Every project key in the site, sorted. Non-project directories and
    /// unreadable names are skipped rather than failing the whole listing.
    pub fn list(&self) -> Result<Vec<ProjectKey>, SiteError> {
        let dir = self.projects_dir();
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => {
                return Err(SiteError::Io {
                    path: dir,
                    message: e.to_string(),
                })
            }
        };
        let mut keys = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.join(crate::project::MARKER).is_file() {
                continue;
            }
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if let Ok(key) = ProjectKey::new(name) {
                keys.push(key);
            }
        }
        keys.sort();
        Ok(keys)
    }

    /// Open the project at `key`, refusing one that still holds accounts.
    pub fn project(&self, key: &ProjectKey) -> Result<Project, SiteError> {
        let path = self.project_path(key);
        if !path.is_dir() {
            return Err(SiteError::NotFound(key.to_string()));
        }
        let project = Project::open(&path).map_err(|e| match e {
            ProjectError::NotAProject(_) => SiteError::NotFound(key.to_string()),
            other => SiteError::Config {
                path: path.clone(),
                message: other.to_string(),
            },
        })?;
        if let Some(members) = project
            .documents()
            .read(Path::new(MEMBERS_FILE))
            .map_err(SiteError::Store)?
        {
            if looks_like_accounts(&members.text) {
                return Err(SiteError::LegacyAccounts {
                    project: path,
                    path: self.project_path(key).join(MEMBERS_FILE),
                });
            }
        }
        Ok(project)
    }

    /// Create a project at `key` with an optional display name.
    pub fn create_project(
        &self,
        key: &ProjectKey,
        name: Option<&str>,
    ) -> Result<Project, SiteError> {
        let path = self.project_path(key);
        if path.exists() {
            return Err(SiteError::KeyInUse(key.to_string()));
        }
        let project = Project::init(&path, name).map_err(|e| SiteError::Io {
            path: path.clone(),
            message: e.to_string(),
        })?;
        Ok(project)
    }

    pub fn is_archived(&self, key: &ProjectKey) -> bool {
        self.config()
            .site
            .archived
            .iter()
            .any(|archived| archived == key)
    }

    /// Archive a project: read-only, interpretations still exportable.
    pub fn archive(&self, key: &ProjectKey) -> Result<(), SiteError> {
        self.ensure_project_exists(key)?;
        if self.is_archived(key) {
            return Ok(());
        }
        let mut config = self.config();
        config.site.archived.push(key.clone());
        config.site.archived.sort();
        self.replace_config(config)
    }

    /// Reverse of [`Site::archive`].
    pub fn unarchive(&self, key: &ProjectKey) -> Result<(), SiteError> {
        self.ensure_project_exists(key)?;
        let mut config = self.config();
        config.site.archived.retain(|archived| archived != key);
        self.replace_config(config)
    }

    /// Delete a project and everything it owns, for good.
    pub fn delete_project(&self, key: &ProjectKey) -> Result<(), SiteError> {
        let path = self.project_path(key);
        if !path.is_dir() {
            return Err(SiteError::NotFound(key.to_string()));
        }
        // `key` is a validated slug, so `path` is always directly under
        // `projects/`; this is a belt-and-braces check that a future caller
        // cannot hand `remove_dir_all` a path outside the site.
        if !path.starts_with(self.projects_dir()) {
            return Err(SiteError::Io {
                path,
                message: "refusing to delete a path outside projects/".to_string(),
            });
        }
        std::fs::remove_dir_all(&path).map_err(|e| SiteError::Io {
            path,
            message: e.to_string(),
        })?;
        if self.is_archived(key) {
            let mut config = self.config();
            config.site.archived.retain(|archived| archived != key);
            self.replace_config(config)?;
        }
        Ok(())
    }

    fn ensure_project_exists(&self, key: &ProjectKey) -> Result<(), SiteError> {
        if self.project_path(key).is_dir() {
            Ok(())
        } else {
            Err(SiteError::NotFound(key.to_string()))
        }
    }

    /// Persist `config` and adopt it as the live value.
    fn replace_config(&self, config: SiteConfig) -> Result<(), SiteError> {
        let expected = match self.manifest.read(Path::new(SITE_MARKER))? {
            Some(document) => Expectation::Version(document.version),
            None => Expectation::Absent,
        };
        self.write_config(&config, &expected)?;
        *self.config.write().expect("site config lock poisoned") = config;
        Ok(())
    }

    fn write_config(
        &self,
        config: &SiteConfig,
        expected: &Expectation,
    ) -> Result<Version, SiteError> {
        let text = toml::to_string_pretty(config).map_err(|e| SiteError::Config {
            path: self.root.join(SITE_MARKER),
            message: e.to_string(),
        })?;
        Ok(self
            .manifest
            .write(Path::new(SITE_MARKER), &text, expected)?)
    }
}

/// Whether a project document is the pre-site `users.json`, which held
/// accounts rather than memberships.
///
/// The old shape always had a top-level `users` array; the membership shape
/// uses `members`. A file with neither (or one that will not parse) is left
/// to the module that reads it, which reports the malformed document rather
/// than the site guessing at it.
fn looks_like_accounts(text: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(text)
        .map(|value| value.get("users").is_some())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn site() -> (tempfile::TempDir, Site) {
        let dir = tempfile::tempdir().unwrap();
        let site = Site::init(dir.path(), Some("Test site")).unwrap();
        (dir, site)
    }

    fn key(name: &str) -> ProjectKey {
        ProjectKey::new(name).unwrap()
    }

    #[test]
    fn init_writes_a_marker_and_projects_dir() {
        let (dir, site) = site();
        assert!(dir.path().join(SITE_MARKER).is_file());
        assert!(dir.path().join(PROJECTS_DIR).is_dir());
        assert_eq!(site.name(), "Test site");
        assert!(site.list().unwrap().is_empty());
    }

    #[test]
    fn open_and_discover_find_the_site() {
        let (dir, _) = site();
        let opened = Site::open(dir.path()).unwrap();
        assert_eq!(opened.name(), "Test site");
        // Discovery works from a file or a subdirectory too.
        let nested = dir.path().join(PROJECTS_DIR);
        let discovered = Site::discover(&nested).unwrap().unwrap();
        assert_eq!(discovered.root(), dir.path());
    }

    #[test]
    fn a_directory_without_a_marker_is_not_a_site() {
        let dir = tempfile::tempdir().unwrap();
        let error = Site::open(dir.path()).unwrap_err();
        assert!(matches!(error, SiteError::NotASite(_)), "{error}");
        assert!(Site::discover(dir.path()).unwrap().is_none());
    }

    #[test]
    fn create_list_and_open_a_project() {
        let (_dir, site) = site();
        let glac = key("glac-2026");
        site.create_project(&glac, Some("Glaciology 2026")).unwrap();
        assert_eq!(site.list().unwrap(), vec![glac.clone()]);
        let project = site.project(&glac).unwrap();
        assert_eq!(
            project.config().project.name.as_deref(),
            Some("Glaciology 2026")
        );
        // A second project at the same key is refused.
        assert!(matches!(
            site.create_project(&glac, None).unwrap_err(),
            SiteError::KeyInUse(_)
        ));
        // And an unknown key is not found.
        assert!(matches!(
            site.project(&key("absent")).unwrap_err(),
            SiteError::NotFound(_)
        ));
    }

    #[test]
    fn archive_is_recorded_and_reversible() {
        let (_dir, site) = site();
        let glac = key("glac-2026");
        site.create_project(&glac, None).unwrap();
        assert!(!site.is_archived(&glac));
        site.archive(&glac).unwrap();
        assert!(site.is_archived(&glac));
        // Re-opened from disk, so the marker carried it.
        let reopened = Site::open(site.root()).unwrap();
        assert!(reopened.is_archived(&glac));
        site.unarchive(&glac).unwrap();
        assert!(!site.is_archived(&glac));
        assert!(matches!(
            site.archive(&key("absent")).unwrap_err(),
            SiteError::NotFound(_)
        ));
    }

    #[test]
    fn delete_removes_the_project_and_its_archive_entry() {
        let (dir, site) = site();
        let glac = key("glac-2026");
        site.create_project(&glac, None).unwrap();
        site.archive(&glac).unwrap();
        site.delete_project(&glac).unwrap();
        assert!(!dir.path().join(PROJECTS_DIR).join("glac-2026").exists());
        assert!(!site.is_archived(&glac));
        assert!(site.list().unwrap().is_empty());
        assert!(matches!(
            site.delete_project(&glac).unwrap_err(),
            SiteError::NotFound(_)
        ));
    }

    #[test]
    fn a_project_that_still_holds_accounts_is_refused() {
        let (_dir, site) = site();
        let glac = key("glac-2026");
        let project = site.create_project(&glac, None).unwrap();
        project
            .documents()
            .write(
                Path::new(MEMBERS_FILE),
                r#"{"users": [{"name": "anna", "role": "admin"}]}"#,
                &Expectation::Absent,
            )
            .unwrap();
        let error = site.project(&glac).unwrap_err();
        assert!(matches!(error, SiteError::LegacyAccounts { .. }), "{error}");
        assert!(
            error.to_string().contains("accounts are server-wide now"),
            "{error}"
        );
    }

    #[test]
    fn a_memberships_file_is_accepted() {
        let (_dir, site) = site();
        let glac = key("glac-2026");
        let project = site.create_project(&glac, None).unwrap();
        project
            .documents()
            .write(
                Path::new(MEMBERS_FILE),
                r#"{"members": [{"name": "anna", "role": "admin", "download": "all"}]}"#,
                &Expectation::Absent,
            )
            .unwrap();
        assert!(site.project(&glac).is_ok());
    }

    #[test]
    fn a_non_project_directory_in_projects_is_ignored() {
        let (dir, site) = site();
        std::fs::create_dir_all(dir.path().join(PROJECTS_DIR).join("scratch")).unwrap();
        assert!(site.list().unwrap().is_empty());
    }
}
