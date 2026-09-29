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

use crate::identity::{ProjectKey, UserId};
use crate::project::members;
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
    /// Deleting a project that has not been archived first.
    NotArchived(String),
    /// A change to the site's accounts was refused or failed.
    Account(accounts::AccountError),
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
            SiteError::NotArchived(key) => write!(
                f,
                "Project '{key}' is not archived. Archive it first: deleting is \
                 the second step, for a project that is already read-only."
            ),
            SiteError::Account(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for SiteError {}

impl From<StoreError> for SiteError {
    fn from(error: StoreError) -> Self {
        SiteError::Store(error)
    }
}

/// What a batch of new accounts is given: a membership in `project`, as
/// `role` with `download`, or nothing when there is no project.
#[derive(Debug, Clone)]
pub struct Grant {
    pub project: Option<ProjectKey>,
    pub role: crate::project::roles::Role,
    pub download: crate::project::roles::DownloadScope,
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
    #[cfg_attr(not(feature = "server"), allow(dead_code))]
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

    /// Create a project at `key` with an optional display name, recording
    /// the account that created it (`None` from the command line).
    pub fn create_project(
        &self,
        key: &ProjectKey,
        name: Option<&str>,
        created_by: Option<&UserId>,
    ) -> Result<Project, SiteError> {
        let path = self.project_path(key);
        if path.exists() {
            return Err(SiteError::KeyInUse(key.to_string()));
        }
        let io = |e: ProjectError| SiteError::Io {
            path: path.clone(),
            message: e.to_string(),
        };
        let project = Project::init(&path, name).map_err(io)?;
        project
            .set_creator(created_by, &chrono::Utc::now().to_rfc3339())
            .map_err(io)?;
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

    /// Delete an archived project and everything it owns, for good.
    ///
    /// Only an archived project may be deleted, so removing one is always
    /// two deliberate steps: archive (read-only, still exportable), then
    /// delete.
    pub fn delete_project(&self, key: &ProjectKey) -> Result<(), SiteError> {
        let path = self.project_path(key);
        if !path.is_dir() {
            return Err(SiteError::NotFound(key.to_string()));
        }
        if !self.is_archived(key) {
            return Err(SiteError::NotArchived(key.to_string()));
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
        let mut config = self.config();
        config.site.archived.retain(|archived| archived != key);
        self.replace_config(config)
    }

    /// The projects that can be opened, each with its memberships.
    ///
    /// A project that still holds accounts is skipped: a site never serves
    /// it, and its file holds no memberships to find. Anything else that
    /// stops a project opening is an error, because a membership hidden in
    /// an unreadable project is one this site cannot account for.
    fn memberships(&self) -> Result<Vec<(ProjectKey, Project)>, SiteError> {
        let mut projects = Vec::new();
        for key in self.list()? {
            match self.project(&key) {
                Ok(project) => projects.push((key, project)),
                Err(SiteError::LegacyAccounts { .. }) => continue,
                Err(e) => return Err(e),
            }
        }
        Ok(projects)
    }

    /// Every name any project has a membership for, whether or not an
    /// account by that name exists.
    ///
    /// A name in here must not be handed out by anyone who does not
    /// administer the whole site: whoever gets it inherits every one of
    /// those memberships.
    #[cfg_attr(not(feature = "server"), allow(dead_code))]
    pub fn member_names(&self) -> Result<std::collections::BTreeSet<UserId>, SiteError> {
        let mut names = std::collections::BTreeSet::new();
        for (_, project) in self.memberships()? {
            let set = members::read_for_access(project.documents());
            names.extend(set.members.into_iter().map(|member| member.name));
        }
        Ok(names)
    }

    /// Remove `name`'s membership from every project, returning the keys it
    /// was removed from.
    ///
    /// Called when an account is deleted, so a later account with the same
    /// name starts with nothing rather than inheriting the old one's roles.
    /// Picks are left alone: they belong to the project's history.
    fn remove_memberships(&self, name: &UserId) -> Result<Vec<ProjectKey>, SiteError> {
        let mut removed = Vec::new();
        for (key, project) in self.memberships()? {
            // Checked first so a project with no membership file does not
            // gain one, which would change its access policy from "never
            // configured" to "configured".
            if members::read_for_access(project.documents())
                .get(name)
                .is_none()
            {
                continue;
            }
            let was_member = members::update(project.documents(), |set| {
                let before = set.members.len();
                set.members.retain(|member| &member.name != name);
                Ok(set.members.len() != before)
            })
            .map_err(|e| SiteError::Io {
                path: self.project_path(&key),
                message: e.to_string(),
            })?;
            if was_member {
                removed.push(key);
            }
        }
        Ok(removed)
    }

    /// Delete the account `name` and its membership in every project,
    /// returning the keys it was removed from.
    ///
    /// Refuses to remove the last server administrator. The memberships go
    /// first: were the account to go first and this then fail, the
    /// memberships would be left for the next account of the same name to
    /// inherit. This way round, a failure leaves an account with fewer
    /// memberships, which is safe to retry.
    pub fn remove_account(&self, name: &UserId) -> Result<Vec<ProjectKey>, SiteError> {
        use accounts::{AccountError, AccountSet};
        let refuse = |set: &AccountSet| match set.get(name) {
            None => Err(AccountError::NotFound(name.to_string())),
            Some(account) if account.server_admin && !set.has_another_admin(name) => {
                Err(AccountError::Rejected(format!(
                    "'{name}' is the only server administrator. Make someone else \
                     one first, or nobody will be able to manage accounts."
                )))
            }
            Some(_) => Ok(()),
        };
        // Checked before the memberships go, so a refused delete changes
        // nothing.
        let (set, _) = accounts::read(self.store())
            .map_err(SiteError::Account)?
            .ok_or_else(|| SiteError::Account(AccountError::NotFound(name.to_string())))?;
        refuse(&set).map_err(SiteError::Account)?;

        let removed_from = self.remove_memberships(name)?;
        accounts::update(self.store(), |set| {
            refuse(set)?;
            set.users.retain(|account| &account.name != name);
            Ok(())
        })
        .map_err(SiteError::Account)?;
        // Their personal settings go with them, site-wide and in every
        // project, so a later account of the same name starts clean. Best
        // effort: a leftover theme is not worth failing a removal over.
        let _ = crate::project::preferences::remove(self.store(), name);
        for (_, project) in self.memberships()? {
            let _ = crate::project::preferences::remove(project.documents(), name);
        }
        Ok(removed_from)
    }

    /// Names for `count` new accounts: numbered after `prefix`
    /// (`student-01`, …), or drawn from the fixed pool when `prefix` is
    /// `None`. Never a name an account has, nor one any project still has a
    /// membership for, since whoever took it would inherit that membership.
    pub fn batch_names(
        &self,
        prefix: Option<&str>,
        count: usize,
    ) -> Result<Vec<UserId>, SiteError> {
        let mut taken = self.member_names()?;
        if let Some((set, _)) = accounts::read(self.store()).map_err(SiteError::Account)? {
            taken.extend(set.users.into_iter().map(|account| account.name));
        }
        match prefix {
            None => accounts::bulk::random_bulk_names(taken.iter(), count),
            Some(prefix) => {
                let start = accounts::bulk::next_bulk_start(taken.iter(), prefix);
                accounts::bulk::bulk_names_after(prefix, count, start)
            }
        }
        .map_err(|e| SiteError::Account(accounts::AccountError::Rejected(e.to_string())))
    }

    /// Create invite-only accounts for `names`, all or none, and return each
    /// one's name, one-time token and expiry.
    ///
    /// Each invite carries `grant`'s membership, which is added when it is
    /// redeemed; with no project, the accounts join nothing.
    pub fn invite_batch(
        &self,
        names: &[UserId],
        grant: &Grant,
    ) -> Result<Vec<(UserId, String, i64)>, SiteError> {
        let now = chrono::Utc::now().timestamp();
        let minted = names
            .iter()
            .map(|name| {
                let (token, invite) = match &grant.project {
                    Some(key) => accounts::invite::mint_for_project(
                        now,
                        key.clone(),
                        grant.role,
                        grant.download,
                    ),
                    None => accounts::invite::mint(now, None, None, None),
                }
                .map_err(|e| SiteError::Account(accounts::AccountError::Rejected(e)))?;
                Ok((name.clone(), token, invite))
            })
            .collect::<Result<Vec<_>, SiteError>>()?;
        accounts::update(self.store(), |set| {
            if let Some((name, _, _)) = minted.iter().find(|(name, _, _)| set.get(name).is_some()) {
                return Err(accounts::AccountError::Duplicate(name.to_string()));
            }
            for (name, _, invite) in &minted {
                let mut account = accounts::Account::new(name.clone(), false);
                account.invite = Some(invite.clone());
                set.users.push(account);
            }
            Ok(())
        })
        .map_err(SiteError::Account)?;
        Ok(minted
            .into_iter()
            .map(|(name, token, invite)| (name, token, invite.expires))
            .collect())
    }

    /// Create accounts for `names` with generated passwords, all or none,
    /// and return each name with its password. The only copy of the
    /// passwords is what this returns; only their hashes are stored.
    ///
    /// With no invite to redeem, `grant`'s membership is added now. Refused
    /// for an administrator role: a shared password handed to someone who
    /// runs a project is a standing key to it.
    ///
    /// Slow on purpose, one Argon2id hash per account; call it off the
    /// async runtime.
    #[cfg(feature = "server")]
    pub fn password_batch(
        &self,
        names: &[UserId],
        grant: &Grant,
    ) -> Result<Vec<(UserId, String)>, SiteError> {
        if accounts::bulk::bulk_risk_advisory(grant.role).is_none() {
            return Err(SiteError::Account(accounts::AccountError::Rejected(
                "Administrator accounts must be created with one-time invite links, \
                 not shared passwords."
                    .to_string(),
            )));
        }
        let generated = names
            .iter()
            .map(|name| {
                let password = accounts::bulk::generate_password().map_err(|e| {
                    SiteError::Account(accounts::AccountError::Rejected(e.to_string()))
                })?;
                let hash = accounts::hash_password(&password).map_err(SiteError::Account)?;
                Ok((name.clone(), password, hash))
            })
            .collect::<Result<Vec<_>, SiteError>>()?;
        accounts::update(self.store(), |set| {
            if let Some((name, _, _)) = generated
                .iter()
                .find(|(name, _, _)| set.get(name).is_some())
            {
                return Err(accounts::AccountError::Duplicate(name.to_string()));
            }
            for (name, _, hash) in &generated {
                let mut account = accounts::Account::new(name.clone(), false);
                account.password_hash = Some(hash.clone());
                set.users.push(account);
            }
            Ok(())
        })
        .map_err(SiteError::Account)?;
        if let Some(key) = &grant.project {
            let project = self.project(key)?;
            members::update(project.documents(), |set| {
                for name in names {
                    set.upsert(name, grant.role, grant.download);
                }
                Ok(())
            })
            .map_err(|e| SiteError::Io {
                path: self.project_path(key),
                message: e.to_string(),
            })?;
        }
        Ok(generated
            .into_iter()
            .map(|(name, password, _)| (name, password))
            .collect())
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
pub fn looks_like_accounts(text: &str) -> bool {
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
        // Canonical, as discovery answers: a temporary directory's path goes
        // through a symlink on macOS and gains a `\\?\` prefix on Windows.
        assert_eq!(
            discovered.root(),
            std::fs::canonicalize(dir.path()).unwrap()
        );
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
        site.create_project(&glac, Some("Glaciology 2026"), None)
            .unwrap();
        assert_eq!(site.list().unwrap(), vec![glac.clone()]);
        let project = site.project(&glac).unwrap();
        assert_eq!(
            project.config().project.name.as_deref(),
            Some("Glaciology 2026")
        );
        // A second project at the same key is refused.
        assert!(matches!(
            site.create_project(&glac, None, None).unwrap_err(),
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
        site.create_project(&glac, None, None).unwrap();
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
    fn only_an_archived_project_may_be_deleted() {
        let (dir, site) = site();
        let glac = key("glac-2026");
        site.create_project(&glac, None, None).unwrap();
        assert!(matches!(
            site.delete_project(&glac).unwrap_err(),
            SiteError::NotArchived(_)
        ));
        assert!(dir.path().join(PROJECTS_DIR).join("glac-2026").is_dir());
    }

    #[test]
    fn delete_removes_the_project_and_its_archive_entry() {
        let (dir, site) = site();
        let glac = key("glac-2026");
        site.create_project(&glac, None, None).unwrap();
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
    fn a_site_is_made_once_and_opened_only_by_a_ridal_that_knows_its_format() {
        let (dir, _site) = site();
        assert!(matches!(
            Site::init(dir.path(), None).unwrap_err(),
            SiteError::AlreadyASite(_)
        ));

        // Written by a newer Ridal: refused rather than misread.
        std::fs::write(
            dir.path().join(SITE_MARKER),
            "[site]\nformat_version = 99\n",
        )
        .unwrap();
        let error = Site::open(dir.path()).unwrap_err();
        assert!(matches!(
            error,
            SiteError::UnsupportedFormat { version: 99, .. }
        ));
        assert!(error.to_string().contains("Upgrade Ridal"), "{error}");

        std::fs::write(dir.path().join(SITE_MARKER), "not toml [").unwrap();
        assert!(matches!(
            Site::open(dir.path()).unwrap_err(),
            SiteError::Config { .. }
        ));
    }

    #[test]
    fn every_site_error_says_what_to_do_in_words() {
        let path = PathBuf::from("/srv/ridal");
        for (error, expected) in [
            (SiteError::NotASite(path.clone()), "ridal site init"),
            (
                SiteError::AlreadyASite(path.clone()),
                "already a Ridal site",
            ),
            (
                SiteError::Io {
                    path: path.clone(),
                    message: "disk full".to_string(),
                },
                "disk full",
            ),
            (
                SiteError::Config {
                    path: path.clone(),
                    message: "bad".to_string(),
                },
                "could not read",
            ),
            (SiteError::NotFound("glac".to_string()), "No project 'glac'"),
            (SiteError::KeyInUse("glac".to_string()), "immutable"),
            (
                SiteError::NotArchived("glac".to_string()),
                "Archive it first",
            ),
        ] {
            let text = error.to_string();
            assert!(text.contains(expected), "{text}");
        }
    }

    #[test]
    fn a_site_with_no_projects_directory_lists_none() {
        let (dir, site) = site();
        std::fs::remove_dir(dir.path().join(PROJECTS_DIR)).unwrap();
        assert!(site.list().unwrap().is_empty());
    }

    #[test]
    fn a_project_that_still_holds_accounts_is_refused() {
        let (_dir, site) = site();
        let glac = key("glac-2026");
        let project = site.create_project(&glac, None, None).unwrap();
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
        let project = site.create_project(&glac, None, None).unwrap();
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
