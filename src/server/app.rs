//! Shared Axum application and typed state (#120).
//!
//! Route handlers here should primarily compose the already-tested
//! catalog (M3) and render-service (M4/M5) components -- no new NetCDF,
//! catalog, or rendering logic belongs in this module.

use std::collections::HashMap;
use std::path::{Path as StdPath, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use axum::routing::get;
use axum::Router;
use tokio::sync::Semaphore;

use super::catalog::{Catalog, CatalogRoot, RevisionId};
use crate::identity::RadargramId;
use crate::server::render_service::{RenderService, RenderServiceConfig};
use crate::source::{AmplitudeSource, SourceReader};

/// One open radargram: its render service plus the metadata needed to
/// answer dataset-detail and viewer-page requests without re-inspecting
/// the file.
pub struct OpenRadargram {
    pub service: Mutex<RenderService>,
    pub shape: (usize, usize),
}

/// What this server permits, independent of who is asking.
///
/// A struct rather than two bare `bool` parameters, which would be
/// adjacent, same-typed and easy to transpose at a call site -- and one of
/// them decides whether passwords may cross a network in the clear.
#[derive(Debug, Clone, Copy)]
pub struct AccessOptions {
    /// `--read-only`: cap every caller at `viewer`, whatever their account
    /// says.
    pub read_only: bool,
    /// Whether a password may be sent to this server at all.
    ///
    /// Decided from the bind address (see [`super::launch`]) and carried
    /// here rather than checked once at startup, because "does this
    /// project have accounts" can become true while the server is running
    /// -- an administrator created on the machine takes effect on the next
    /// request, so a guard sampled at boot would be bypassed by exactly
    /// the sequence that makes it matter.
    pub allow_password_login: bool,
}

impl Default for AccessOptions {
    /// Writable, and logins permitted. The loopback case, and what tests
    /// want: a bind that is either genuinely local or behind a proxy.
    fn default() -> Self {
        Self {
            read_only: false,
            allow_password_login: true,
        }
    }
}

pub struct AppState {
    /// How render services are configured, kept so [`Self::rediscover`]
    /// can open one for a radargram that arrives after startup.
    render_config: RenderServiceConfig,
    /// Held for the whole of any operation that changes which radargrams
    /// exist (#147).
    ///
    /// Add and remove are each a sequence — measure the project, write,
    /// check for a collision, install, rediscover — and every step of it
    /// reads state the other steps change. Run two at once and each
    /// individually correct sequence produces a wrong result together: two
    /// uploads both see room for one file, both see no id collision and
    /// the second rename replaces the first, and two rediscoveries
    /// complete out of order so the newer catalog is replaced by one built
    /// from an older disk.
    ///
    /// One lock rather than a check at each step, because the steps are not
    /// individually fixable: what each needs is that nothing else changed
    /// in between, which is what a lock says and a check cannot.
    ///
    /// A `std` mutex held across `.await` would not do — this is
    /// `tokio::sync` so an upload can be awaited while holding it.
    lifecycle: tokio::sync::Mutex<()>,
    /// Every place radargrams are found, the served tree first (#147).
    ///
    /// A list rather than one path because a project may point at archives
    /// outside itself. The first is the writable upper layer; the rest are
    /// read-only lower ones, and the upper layer wins where they overlap.
    pub roots: Vec<CatalogRoot>,
    /// The catalog and its render services, swappable at runtime (#147).
    ///
    /// One lock over both, not one each. Two locks taken in a fixed order
    /// prevent deadlock; they do not prevent *tearing*, and a reader that
    /// took the catalog from one generation and a render service from the
    /// next would render a radargram at the wrong shape, or be told a
    /// freshly added one is unavailable.
    ///
    /// `Arc` rather than the value so a reader takes a snapshot in O(1) and
    /// drops the guard immediately. That matters more than it looks: these
    /// are `async` handlers, a `std` read guard is not `Send`, and holding
    /// one across an `.await` would not compile -- so the choice is between
    /// copy-on-write and cloning a hundred entries per request. Mutation
    /// builds a whole new snapshot and swaps the pointer, which also means
    /// a request that started before an edit finishes against the state it
    /// started with rather than seeing it change mid-flight.
    snapshot: RwLock<Arc<CatalogSnapshot>>,
    /// The project this catalog belongs to, when it belongs to one.
    ///
    /// `None` is the read-only case Ridal has always supported: a bare
    /// directory of `.nc` files, or a single file, with nowhere to save
    /// anything. Every write route checks this rather than assuming.
    pub project: Option<crate::project::Project>,
    /// What this server permits regardless of who is asking.
    ///
    /// `read_only` is a cap on the caller's role rather than a separate
    /// switch on the write routes, because "what may this request do" has
    /// exactly one answer -- the caller's effective role -- and a second,
    /// parallel gate is how the two drift apart. See
    /// [`super::auth::Caller`].
    pub access: AccessOptions,
    /// The key that signs session cookies, loaded on first use.
    ///
    /// Lazy so a project that never authenticates never grows a
    /// `session.key`, and behind a lock rather than a `OnceLock` so a
    /// failure to read it is retried on the next login instead of being
    /// cached forever.
    session_key: Mutex<Option<super::auth::SessionKey>>,
    /// Bounds how many renders may be in flight at once, across every
    /// radargram, sized from `--n-workers`.
    ///
    /// Rendering is CPU-bound and runs on `spawn_blocking` threads, whose
    /// pool tokio sizes at 512 by default -- far more than is useful for
    /// work that is already competing for cores, and enough that a card
    /// grid requesting one overview per catalog entry could start
    /// hundreds of simultaneous renders, each holding its own source
    /// band in memory. The permit is acquired *before* spawning so a
    /// client that disconnects while queued never starts one at all.
    pub render_permits: Arc<Semaphore>,
}

impl AppState {
    /// Discover the catalog under `root` and eagerly open a
    /// [`RenderService`] for every entry. Eager rather than lazy: the
    /// issue's target catalog size is ~100 files (#122/#123), and eager
    /// construction means a broken file surfaces as a clear startup
    /// warning rather than a request-time surprise.
    ///
    /// `root` is canonicalized once here, the single point every later
    /// filesystem operation on it (`Catalog::discover`, every
    /// `resolve_absolute_path` call, and the stored `self.root` used by
    /// `absolute_path()`) flows from. This is the CLI's own
    /// `ridal gui <PATH>` / `ridal server start <PATH>` argument, fixed
    /// once at process startup and never influenced by an HTTP request --
    /// `AppState::build` has exactly one production caller (`launch.rs`).
    /// Canonicalizing it anyway, rather than trusting that, is what CodeQL's
    /// path-injection analysis recognises as validating a path before use,
    /// and it's a genuine improvement on its own merits regardless: a
    /// nonexistent root now fails clearly at startup instead of silently
    /// producing an empty catalog, and every later comparison against it
    /// (this function's own containment check below) is symlink-resolved
    /// and consistent.
    /// `project` is `None` for a bare directory or single file, which has
    /// nowhere to write and is therefore read-only whatever `access` says.
    pub fn build_with_project(
        root: &StdPath,
        config: &RenderServiceConfig,
        project: Option<crate::project::Project>,
        access: AccessOptions,
    ) -> Result<Self, String> {
        let root = root
            .canonicalize()
            .map_err(|e| format!("Invalid catalog root {}: {e}", root.display()))?;
        let root_is_file = root.is_file();
        let root = root.as_path();
        // What the project says over what the files say (#145). Read
        // leniently: a hand-broken overrides document should cost the
        // project its labels, not every page it serves.
        let overrides = project
            .as_ref()
            .map(|p| crate::project::overrides::read_lenient(p.documents()))
            .unwrap_or_default();

        // The served tree first, then anything `[radargrams] roots` points
        // at (#147). What makes a root writable is being *inside the
        // project*, not being the path the CLI was given: `ridal gui
        // project/radargrams/line.nc` discovers the project upwards, and
        // `project/radargrams` does not start with that file — so keying
        // off the served path marked the project's own directory read-only
        // and every radargram in it as not in the project.
        let project_root = project
            .as_ref()
            .map(|p| p.root().to_path_buf())
            .and_then(|p| p.canonicalize().ok());
        let owned = |path: &StdPath| {
            project_root
                .as_ref()
                .is_some_and(|inside| path.starts_with(inside))
        };

        // The project's own radargram directories, canonicalized. A file
        // sitting directly in one of these is ungrouped: `radargrams/` is
        // where a project keeps its files, not a group anyone chose. See
        // `CatalogRoot::group_bases`.
        let declared: Vec<PathBuf> = project
            .as_ref()
            .map(|p| {
                p.radargram_roots()
                    .into_iter()
                    .filter_map(|d| d.canonicalize().ok())
                    .collect()
            })
            .unwrap_or_default();
        let bases_under = |root: &StdPath| -> Vec<PathBuf> {
            declared
                .iter()
                .filter(|d| d.starts_with(root))
                .cloned()
                .collect()
        };

        let mut roots = vec![CatalogRoot {
            path: root.to_path_buf(),
            is_file: root_is_file,
            writable: owned(root),
            group_bases: bases_under(root),
        }];
        if let Some(project) = &project {
            for extra in project.radargram_roots() {
                // Canonicalized here, next to the containment checks it
                // feeds, so a symlinked root cannot look external and then
                // resolve back inside -- or the reverse.
                let Ok(extra) = extra.canonicalize() else {
                    eprintln!(
                        "Warning: radargram root {} does not exist and was skipped.",
                        extra.display()
                    );
                    continue;
                };
                // Already covered by the served tree. Scanning it twice
                // would make every radargram in it its own duplicate.
                if extra.starts_with(root) {
                    continue;
                }
                let is_file = extra.is_file();
                let writable = owned(&extra);
                // The root *is* a declared directory, so its own name is
                // structure too: an external archive listed under
                // `[radargrams] roots` groups by what is inside it, not by
                // what the directory happens to be called.
                let group_bases = bases_under(&extra);
                roots.push(CatalogRoot {
                    path: extra,
                    is_file,
                    writable,
                    group_bases,
                });
            }
        }

        let catalog = Catalog::discover_roots(&roots, &overrides);
        let mut radargrams = HashMap::new();

        for entry in &catalog.entries {
            let Some(entry_root) = roots.get(entry.root) else {
                continue;
            };
            let absolute_path = match Self::resolve_absolute_path(entry_root, entry) {
                Ok(path) => path,
                Err(e) => {
                    eprintln!(
                        "Warning: refusing unsafe catalog path {}: {e}",
                        entry.relative_path
                    );
                    continue;
                }
            };
            let reader = match SourceReader::open(&absolute_path) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!(
                        "Warning: could not open {} for rendering: {e}",
                        entry.relative_path
                    );
                    continue;
                }
            };
            let shape = reader.shape();
            let revision_id: RevisionId = entry.revision_id.clone();
            let service = RenderService::new(reader, revision_id, config);
            radargrams.insert(
                entry.radargram_id.as_str().to_string(),
                Arc::new(OpenRadargram {
                    service: Mutex::new(service),
                    shape,
                }),
            );
        }

        Ok(Self {
            roots,
            render_config: *config,
            lifecycle: tokio::sync::Mutex::new(()),
            snapshot: RwLock::new(Arc::new(CatalogSnapshot {
                catalog,
                radargrams,
            })),
            access,
            session_key: Mutex::new(None),
            project,
            // `.max(1)`: a zero-permit semaphore would deadlock every
            // render forever. The CLI rejects `--n-workers 0` with a
            // clear message, so this only guards programmatic callers.
            render_permits: Arc::new(Semaphore::new(config.n_workers.max(1))),
        })
    }

    /// Resolves a catalog entry beneath the canonical catalog root.
    ///
    /// Rejects absolute paths, parent-directory components, and paths whose
    /// canonical form escapes the root, including through symlinks.
    ///
    /// `CatalogRoot::is_file` was decided once against a freshly
    /// canonicalized path rather than being re-stated here: this function
    /// then only ever touches the filesystem through the
    /// join-then-canonicalize sequence applied to `candidate`, which is the
    /// pattern CodeQL's path-injection analysis recognises as validated. A
    /// raw `is_file()` inside kept getting re-flagged even against a
    /// canonicalized root, because the canonicalization happened in a
    /// different function from the sink and CodeQL does not credit a
    /// barrier it cannot see next to the check it guards.
    fn resolve_absolute_path(
        root: &CatalogRoot,
        entry: &super::catalog::CatalogEntry,
    ) -> Result<PathBuf, String> {
        if root.is_file {
            return Ok(root.path.clone());
        }
        let root = root.path.as_path();

        let mut candidate = root.to_path_buf();
        for component in StdPath::new(&entry.relative_path).components() {
            match component {
                std::path::Component::Normal(part) => candidate.push(part),
                _ => {
                    return Err(format!(
                        "path contains a disallowed component: {}",
                        entry.relative_path
                    ));
                }
            }
        }

        let candidate = candidate
            .canonicalize()
            .map_err(|e| format!("could not resolve {}: {e}", candidate.display()))?;

        if !candidate.starts_with(root) {
            return Err(format!(
                "resolved path escapes catalog root: {}",
                candidate.display()
            ));
        }

        Ok(candidate)
    }

    /// The absolute filesystem path for a catalog entry. Never exposed to
    /// HTTP clients directly (#122: "keep filesystem paths internal") --
    /// only used server-side, e.g. to re-open a file for track reading.
    pub fn absolute_path(&self, entry: &super::catalog::CatalogEntry) -> Result<PathBuf, String> {
        let root = self
            .roots
            .get(entry.root)
            .ok_or_else(|| format!("entry names root {}, which is not served", entry.root))?;
        Self::resolve_absolute_path(root, entry)
    }

    /// Exclusive access for an operation that changes which radargrams
    /// exist. See [`AppState::lifecycle`].
    pub async fn lifecycle_lock(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.lifecycle.lock().await
    }

    /// Drop the render service for one radargram, so its file can be
    /// deleted.
    ///
    /// Windows refuses to unlink a file that is still open, and a served
    /// radargram's `RenderService` holds a NetCDF handle on it — so
    /// removing a perfectly ordinary radargram would fail there and
    /// nowhere else. Unix would have allowed the unlink and quietly kept
    /// the handle alive, which is not better, only quieter.
    ///
    /// Returns once no snapshot references the service. A render already in
    /// flight holds its own `Arc` and finishes against the file it started
    /// on, which is the same rule every other reader here follows; the
    /// delete can fail on Windows in that window, and reporting that is
    /// honest — the file really is in use.
    pub fn close_radargram(&self, radargram_id: &str) {
        let existing = self.catalog();
        let mut radargrams = existing.open_radargrams();
        radargrams.remove(radargram_id);
        self.replace_catalog(Catalog::clone(&existing), radargrams);
    }

    /// Re-read the roots and rebuild the catalog and its render services.
    ///
    /// What add and remove need, and what an override edit does *not*:
    /// re-resolution answers "what does the project say about these files",
    /// and this answers "which files are there". One re-reads a document,
    /// the other walks the disk.
    ///
    /// Render services are carried over for every radargram whose revision
    /// is unchanged. Reopening all of them because one was added would
    /// throw away every warm cache in the project, and the revision
    /// fingerprint is exactly the question "is this the same file" — a
    /// radargram that was replaced gets a new one and is reopened.
    pub fn rediscover(&self) -> Result<(), String> {
        let config = &self.render_config;
        let overrides = self
            .project
            .as_ref()
            .map(|p| crate::project::overrides::read_lenient(p.documents()))
            .unwrap_or_default();
        let catalog = Catalog::discover_roots(&self.roots, &overrides);

        let existing = self.catalog();
        let mut radargrams = HashMap::new();
        for entry in &catalog.entries {
            let key = entry.radargram_id.as_str().to_string();
            let unchanged = existing
                .find_entry(&key)
                .is_some_and(|old| old.revision_id == entry.revision_id);
            if unchanged {
                if let Some(open) = existing.radargram(&key) {
                    radargrams.insert(key, open);
                    continue;
                }
            }
            let Some(root) = self.roots.get(entry.root) else {
                continue;
            };
            let Ok(path) = Self::resolve_absolute_path(root, entry) else {
                continue;
            };
            let Ok(reader) = SourceReader::open(&path) else {
                // Skipped rather than fatal, exactly as at startup: one
                // unreadable file must not cost the whole catalog.
                eprintln!(
                    "Warning: could not open {} for rendering",
                    entry.relative_path
                );
                continue;
            };
            let shape = reader.shape();
            let service = RenderService::new(reader, entry.revision_id.clone(), config);
            radargrams.insert(
                key,
                Arc::new(OpenRadargram {
                    service: Mutex::new(service),
                    shape,
                }),
            );
        }

        self.replace_catalog(catalog, radargrams);
        Ok(())
    }

    /// Whether Ridal may write to the root this entry came from.
    ///
    /// False for every external root, always. The project is the writable
    /// upper layer and an external archive is a read-only lower one -- not
    /// "unless an admin unlocks it", never -- so the same UI can be offered
    /// over both without a wrong click there meaning something different.
    pub fn is_writable(&self, entry: &super::catalog::CatalogEntry) -> bool {
        self.roots.get(entry.root).is_some_and(|root| root.writable)
    }

    /// The key that signs this project's session cookies, creating it on
    /// first use.
    ///
    /// Errors rather than returning `None` for a catalog with no project:
    /// nothing should be asking for a session key there, and answering
    /// "there is no key" would read as "this cookie is fine".
    pub fn session_key(&self) -> Result<super::auth::SessionKey, String> {
        let project = self
            .project
            .as_ref()
            .ok_or_else(|| "this catalog is not a project, so it has no sessions".to_string())?;
        let mut guard = self
            .session_key
            .lock()
            .map_err(|_| "the session key lock was poisoned by a panic".to_string())?;
        if let Some(key) = guard.as_ref() {
            return Ok(key.clone());
        }
        let key = super::auth::project_session_key(project)?;
        *guard = Some(key.clone());
        Ok(key)
    }
    /// The catalog and its services as they are right now.
    ///
    /// Every read goes through here rather than through a field, so there
    /// is one place where "which state is this request working against" is
    /// decided -- and the answer is fixed for the rest of the request even
    /// if an edit lands meanwhile. Ask once and keep it; asking twice in
    /// one handler is how two halves of a page come to disagree, and how a
    /// render ends up using one generation's entry with the next
    /// generation's service.
    pub fn catalog(&self) -> Arc<CatalogSnapshot> {
        self.snapshot
            .read()
            .map(|guard| Arc::clone(&guard))
            // A poisoned lock means some other request panicked while
            // holding it. The snapshot it left is still a whole, valid
            // snapshot -- swaps replace the pointer, so there is no
            // half-written state to inherit -- and failing every later read
            // over it would turn one panic into an unusable server.
            .unwrap_or_else(|poisoned| Arc::clone(&poisoned.into_inner()))
    }

    /// Replace the catalog and the render services that go with it.
    ///
    /// Both together and in one write, because a catalog naming a radargram
    /// with no open service renders an error card, and a service with no
    /// catalog entry is unreachable.
    #[cfg_attr(
        not(test),
        allow(
            dead_code,
            reason = "the callers arrive with catalog overrides (#145) and \
                      add/remove (#147); the lock lands first so those are \
                      about their features rather than about this"
        )
    )]
    pub fn replace_catalog(
        &self,
        catalog: Catalog,
        radargrams: HashMap<String, Arc<OpenRadargram>>,
    ) {
        let mut guard = match self.snapshot.write() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        *guard = Arc::new(CatalogSnapshot {
            catalog,
            radargrams,
        });
    }
}

/// One generation of what the server is serving.
///
/// The catalog and the render services are one value rather than two
/// fields because they only make sense together: an entry describes a file
/// and the service is what reads it, so a reader holding one from each
/// generation could render at the wrong shape, or declare a freshly added
/// radargram unavailable.
///
/// `Deref` to the catalog so the many places that only want an entry read
/// as if they had a catalog, while the three that also need a render
/// service get a matching one out of the same value.
pub struct CatalogSnapshot {
    catalog: Catalog,
    radargrams: HashMap<String, Arc<OpenRadargram>>,
}

impl std::ops::Deref for CatalogSnapshot {
    type Target = Catalog;

    fn deref(&self) -> &Catalog {
        &self.catalog
    }
}

impl CatalogSnapshot {
    /// The open render service for one radargram of *this* generation.
    ///
    /// `Arc` so a handler holds its own reference for as long as the render
    /// takes. A swap landing meanwhile does not disturb it: the render
    /// finishes against the file it started on, which is the same rule the
    /// catalog entry beside it already follows.
    pub fn radargram(&self, radargram_id: &str) -> Option<Arc<OpenRadargram>> {
        self.radargrams.get(radargram_id).map(Arc::clone)
    }

    /// Every open render service, for carrying across a swap that does not
    /// change any file's contents.
    ///
    /// The caller is the catalog-overrides refresh (#145): an override
    /// changes labels and grouping, never a file's contents, so reopening
    /// every radargram would throw away every warm cache for nothing.
    pub fn open_radargrams(&self) -> HashMap<String, Arc<OpenRadargram>> {
        self.radargrams.clone()
    }
}

impl Catalog {
    pub fn find_entry(&self, radargram_id: &str) -> Option<&super::catalog::CatalogEntry> {
        self.entries
            .iter()
            .find(|e| e.radargram_id.as_str() == radargram_id)
    }

    /// All entries sharing `group`, ordered like the catalog itself.
    /// `group == NO_GROUP_ID` matches entries with no group at all,
    /// rather than a literal group id -- see [`NO_GROUP_ID`].
    pub fn entries_in_group(&self, group: &str) -> Vec<&super::catalog::CatalogEntry> {
        self.entries
            .iter()
            .filter(|e| {
                if group == NO_GROUP_ID {
                    e.group_id.is_none()
                } else {
                    e.group_id.as_ref().is_some_and(|g| g.as_str() == group)
                }
            })
            .collect()
    }
}

/// What a merged download covers: one group, or the whole catalog.
///
/// The two differ only in which entries they select and what the file is
/// called. Keeping that difference in one type means every merged product
/// -- level 2 points, tracks, and whatever is added next -- is implemented
/// once and offered at both scopes, instead of a catalog copy drifting from
/// the group original.
pub enum MergeScope {
    /// Every radargram the server knows about, groups and ungrouped alike.
    Catalog,
    /// One group id, or [`NO_GROUP_ID`] for the ungrouped pseudo-group.
    Group(String),
}

impl MergeScope {
    pub fn entries<'a>(&self, catalog: &'a Catalog) -> Vec<&'a super::catalog::CatalogEntry> {
        match self {
            Self::Catalog => catalog.entries.iter().collect(),
            Self::Group(id) => catalog.entries_in_group(id),
        }
    }

    /// What a merged download covers, and how many members it leaves out.
    ///
    /// "Everything in this project" quietly including a radargram somebody
    /// unlisted is a surprise, so they are left out -- and counted, because
    /// a merged file that silently omits members looks complete. The caller
    /// puts the count in the `Warning` header beside the other caveats.
    pub fn listed_entries<'a>(
        &self,
        catalog: &'a Catalog,
    ) -> (Vec<&'a super::catalog::CatalogEntry>, usize) {
        let all = self.entries(catalog);
        let total = all.len();
        let listed: Vec<_> = all.into_iter().filter(|e| !e.unlisted).collect();
        let omitted = total - listed.len();
        (listed, omitted)
    }

    /// Leading component of the download's filename. A slug in both cases:
    /// group ids are validated slugs, and `catalog` is a fixed literal, so
    /// neither can carry a quote into a `Content-Disposition` header.
    pub fn slug(&self) -> &str {
        match self {
            Self::Catalog => "catalog",
            Self::Group(id) => id,
        }
    }

    /// Names the scope inside a sentence, for error messages.
    pub fn describe(&self) -> String {
        match self {
            Self::Catalog => "this catalog".to_string(),
            Self::Group(id) => format!("group '{id}'"),
        }
    }

    /// Error code when the scope selects no radargram at all. Distinct per
    /// scope because the causes are different: a group id that matches
    /// nothing is a bad request in spirit, while an empty catalog is a
    /// server that was pointed at a directory with nothing in it.
    pub fn empty_code(&self) -> &'static str {
        match self {
            Self::Catalog => "catalog_empty",
            Self::Group(_) => "group_not_found",
        }
    }
}

/// Reserved id for the "Ungrouped" pseudo-group on the index page and its
/// `/api/v1/groups/{id}/tracks` map. Safe by construction: `GroupId`
/// validation (`identity.rs::validate_slug`) rejects any id starting with
/// `_`, so no explicit `--group-id` or directory-derived slug can ever
/// collide with it -- the same guarantee `routes.rs`'s synthetic
/// `__revision_id`/`__shape` metadata keys rely on.
///
/// This is a presentation concept only: `CatalogEntry::group_id` itself
/// stays `None` for a genuinely ungrouped radargram. Widening that to a
/// real `GroupId` would leak into the viewer, which correctly treats
/// `None` as "no group" (no banner suffix, no sibling-track fetch) --
/// every ungrouped radargram in the catalog is not one group.
pub const NO_GROUP_ID: &str = "_none";

/// Build the complete Axum application over `state`.
pub fn build_router(state: std::sync::Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(super::routes::index_page))
        .route("/view/{radargram_id}", get(super::routes::viewer_page))
        .route("/static/leaflet.js", get(super::assets::leaflet_js))
        .route("/static/leaflet.css", get(super::assets::leaflet_css))
        .route("/static/app.css", get(super::assets::app_css))
        .route("/static/app.js", get(super::assets::app_js))
        .route("/static/index.js", get(super::assets::index_js))
        .route("/static/viewer.js", get(super::assets::viewer_js))
        .route("/static/picker.js", get(super::assets::picker_js))
        .route(
            "/static/images/marker-icon.png",
            get(super::assets::marker_icon),
        )
        .route(
            "/static/images/marker-icon-2x.png",
            get(super::assets::marker_icon_2x),
        )
        .route(
            "/static/images/marker-shadow.png",
            get(super::assets::marker_shadow),
        )
        .route("/static/images/layers.png", get(super::assets::layers_png))
        .route(
            "/static/images/layers-2x.png",
            get(super::assets::layers_2x_png),
        )
        .route("/static/images/logo.svg", get(super::assets::logo_svg))
        .route("/favicon.ico", get(super::assets::favicon))
        .route("/api/v1/health", get(super::routes::health))
        // Authentication. The write routes below did not change shape when
        // this arrived (#131): the path still names the user, and only the
        // body of `current_user` moved.
        .route("/login", get(super::auth_routes::login_page))
        .route("/invite/{token}", get(super::auth_routes::invite_page))
        .route("/static/login.js", get(super::assets::login_js))
        .route("/api/v1/auth/me", get(super::auth_routes::me))
        .route(
            "/api/v1/auth/login",
            axum::routing::post(super::auth_routes::login),
        )
        .route(
            "/api/v1/auth/logout",
            axum::routing::post(super::auth_routes::logout),
        )
        .route(
            "/api/v1/auth/invite",
            axum::routing::post(super::auth_routes::redeem_invite),
        )
        .route(
            "/api/v1/users",
            get(super::auth_routes::list_users).post(super::auth_routes::create_user),
        )
        .route(
            "/api/v1/users/{name}",
            axum::routing::put(super::auth_routes::update_user)
                .delete(super::auth_routes::delete_user),
        )
        .route(
            "/api/v1/users/{name}/invite",
            axum::routing::post(super::auth_routes::reissue_invite),
        )
        .route(
            "/api/v1/access",
            axum::routing::put(super::auth_routes::put_access),
        )
        .route(
            "/api/v1/preferences",
            get(super::auth_routes::get_preferences).put(super::auth_routes::put_preferences),
        )
        .route("/layers", get(super::routes::layers_page))
        .route("/settings", get(super::routes::settings_page))
        .route("/static/settings.js", get(super::assets::settings_js))
        .route(
            "/api/v1/project/settings",
            get(super::interp_routes::get_settings).put(super::interp_routes::put_settings),
        )
        .route("/static/layers.js", get(super::assets::layers_js))
        .route(
            "/api/v1/layers",
            get(super::interp_routes::get_layers).put(super::interp_routes::put_layers),
        )
        .route(
            "/api/v1/layers/usage",
            get(super::interp_routes::layer_usage),
        )
        .route(
            "/api/v1/datasets/{radargram_id}/interpretations",
            get(super::interp_routes::list_interpretations),
        )
        .route(
            "/api/v1/catalog/track.geojson",
            get(super::routes::catalog_track_geojson),
        )
        .route(
            "/api/v1/catalog/level2",
            get(super::interp_routes::catalog_level2),
        )
        .route(
            "/api/v1/groups/{group}/track.geojson",
            get(super::routes::group_track_geojson),
        )
        .route(
            "/api/v1/groups/{group}/level2",
            get(super::interp_routes::group_level2),
        )
        .route(
            "/api/v1/datasets/{radargram_id}/download",
            get(super::routes::dataset_download),
        )
        .route(
            "/api/v1/datasets/{radargram_id}/track.geojson",
            get(super::routes::dataset_track_geojson),
        )
        .route(
            "/api/v1/datasets/{radargram_id}/views/{view}/image",
            get(super::routes::dataset_image),
        )
        .route(
            "/api/v1/datasets/{radargram_id}/interpretations/{user}/raw",
            get(super::interp_routes::get_interpretation_raw),
        )
        .route(
            "/api/v1/datasets/{radargram_id}/interpretations/{user}/level2",
            get(super::interp_routes::interpretation_level2),
        )
        .route(
            "/api/v1/datasets/{radargram_id}/interpretations/{user}",
            get(super::interp_routes::get_interpretation)
                .put(super::interp_routes::put_interpretation)
                .delete(super::interp_routes::delete_interpretation),
        )
        .route("/api/v1/profiles", get(super::routes::list_profiles))
        .route(
            "/api/v1/datasets",
            get(super::routes::list_datasets).post(super::lifecycle_routes::upload_dataset),
        )
        .route(
            "/api/v1/datasets/{radargram_id}/restore",
            axum::routing::post(super::lifecycle_routes::restore_dataset),
        )
        .route(
            "/api/v1/catalog/ignored",
            get(super::lifecycle_routes::list_ignored),
        )
        .route(
            "/api/v1/datasets/{radargram_id}/properties",
            get(super::overrides_routes::get_properties)
                .put(super::overrides_routes::put_properties),
        )
        .route(
            "/api/v1/datasets/{radargram_id}",
            get(super::routes::dataset_detail).delete(super::lifecycle_routes::remove_dataset),
        )
        .route(
            "/api/v1/datasets/{radargram_id}/track",
            get(super::routes::dataset_track),
        )
        .route(
            "/api/v1/datasets/{radargram_id}/attributes",
            get(super::routes::dataset_attributes),
        )
        .route(
            "/api/v1/datasets/{radargram_id}/axes",
            get(super::routes::dataset_axes),
        )
        .route(
            "/api/v1/groups/{group}/tracks",
            get(super::routes::group_tracks),
        )
        .route(
            "/api/v1/groups/{group}/properties",
            get(super::overrides_routes::get_group_properties)
                .put(super::overrides_routes::put_group_properties),
        )
        .route(
            "/api/v1/datasets/{radargram_id}/views/{view}/overview",
            get(super::routes::overview_image),
        )
        .route(
            "/api/v1/datasets/{radargram_id}/views/{view}/chunks/{profile}/{x}/{y}",
            get(super::routes::chunk_image),
        )
        // Every route above is reached through this, so identity is resolved
        // exactly once per request and a route added later cannot forget to
        // ask who is calling. It is also the only place that can enforce
        // "this project requires a login to read", which is a property of
        // the whole site rather than of any one handler.
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            super::auth::middleware,
        ))
        .with_state(state)
}

/// A radargram ID from the URL is validated the same way an explicit
/// `--radargram-id` would be, so a malformed ID (never a real dataset)
/// fails fast with a clear reason rather than a generic "not found".
pub fn validate_radargram_id(raw: &str) -> Result<RadargramId, String> {
    RadargramId::new(raw)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use serde_json::Value;
    use tower::ServiceExt;

    fn write_test_nc(path: &StdPath, radargram_id: &str) {
        let mut file = netcdf::create(path).unwrap();
        file.add_dimension("y", 20).unwrap();
        file.add_dimension("x", 300).unwrap();
        let mut var = file.add_variable::<f32>("data", &["y", "x"]).unwrap();
        let data: Vec<f32> = (0..(20 * 300)).map(|i| (i % 100) as f32).collect();
        var.put_values(&data, ..).unwrap();
        file.add_attribute("ridal_processing_datetime", "2020-01-01T00:00:00Z")
            .unwrap();
        file.add_attribute("ridal_version", "ridal version 0.0.0 by test")
            .unwrap();
        file.add_attribute("ridal_radargram_id", radargram_id)
            .unwrap();
    }

    /// Like `write_test_nc`, but with real track variables (crs, easting,
    /// northing, time) so `dataset_track`/`group_tracks` have something to
    /// read, and an optional group.
    fn write_test_nc_with_track(path: &StdPath, radargram_id: &str, group: Option<&str>) {
        let n_traces = 50;
        let mut file = netcdf::create(path).unwrap();
        file.add_dimension("y", 5).unwrap();
        file.add_dimension("x", n_traces).unwrap();
        let mut data_var = file.add_variable::<f32>("data", &["y", "x"]).unwrap();
        data_var
            .put_values(&vec![1.0f32; 5 * n_traces], ..)
            .unwrap();

        let mut easting_var = file.add_variable::<f64>("easting", &["x"]).unwrap();
        let easting: Vec<f64> = (0..n_traces).map(|i| 500000.0 + i as f64).collect();
        easting_var.put_values(&easting, ..).unwrap();

        let mut northing_var = file.add_variable::<f64>("northing", &["x"]).unwrap();
        northing_var
            .put_values(&vec![8_000_000.0f64; n_traces], ..)
            .unwrap();

        let mut time_var = file.add_variable::<f64>("time", &["x"]).unwrap();
        let time: Vec<f64> = (0..n_traces).map(|i| i as f64 * 0.1).collect();
        time_var.put_values(&time, ..).unwrap();

        file.add_attribute("crs", "EPSG:32633").unwrap();
        file.add_attribute("ridal_processing_datetime", "2020-01-01T00:00:00Z")
            .unwrap();
        file.add_attribute("ridal_version", "ridal version 0.0.0 by test")
            .unwrap();
        file.add_attribute("ridal_radargram_id", radargram_id)
            .unwrap();
        if let Some(group) = group {
            file.add_attribute("ridal_group_name", group).unwrap();
            file.add_attribute("ridal_group_id", group).unwrap();
        }
    }

    fn test_app(dir: &StdPath) -> Router {
        let config = RenderServiceConfig::default();
        let state = std::sync::Arc::new(
            AppState::build_with_project(dir, &config, None, AccessOptions::default()).unwrap(),
        );
        build_router(state)
    }

    /// `test_app`, but keeping the state so a test can inspect the render
    /// cache afterwards, and with a caller-chosen `n_workers`.
    fn test_app_with_state(dir: &StdPath, n_workers: usize) -> (Router, Arc<AppState>) {
        let config = RenderServiceConfig {
            n_workers,
            ..RenderServiceConfig::default()
        };
        let state = std::sync::Arc::new(
            AppState::build_with_project(dir, &config, None, AccessOptions::default()).unwrap(),
        );
        (build_router(state.clone()), state)
    }

    async fn get(app: &Router, uri: &str) -> (StatusCode, axum::body::Bytes) {
        let response = app
            .clone()
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, bytes)
    }

    #[tokio::test]
    async fn health_route_returns_ok() {
        let dir = tempfile::tempdir().unwrap();
        let app = test_app(dir.path());
        let (status, body) = get(&app, "/api/v1/health").await;
        assert_eq!(status, StatusCode::OK);
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["status"], "ok");
    }

    #[test]
    #[test_retry::retry]
    #[serial_test::serial(netcdf)]
    fn full_route_suite_against_a_real_catalog() {
        // A single #[test] (not #[tokio::test]) driving a manually built
        // runtime, so every route in this suite shares one netcdf-serial
        // guard -- otherwise each #[tokio::test] would need its own,
        // fighting the file-per-test isolation this suite wants to test.
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let dir = tempfile::tempdir().unwrap();
            write_test_nc(&dir.path().join("a.nc"), "route-test-a");
            let app = test_app(dir.path());

            // Catalog listing.
            let (status, body) = get(&app, "/api/v1/datasets").await;
            assert_eq!(status, StatusCode::OK);
            let json: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(json["entries"].as_array().unwrap().len(), 1);
            assert_eq!(json["entries"][0]["radargram_id"], "route-test-a");

            // Dataset detail: known and unknown.
            let (status, _) = get(&app, "/api/v1/datasets/route-test-a").await;
            assert_eq!(status, StatusCode::OK);
            let (status, body) = get(&app, "/api/v1/datasets/does-not-exist").await;
            assert_eq!(status, StatusCode::NOT_FOUND);
            let json: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(json["error"]["code"], "dataset_not_found");

            // An invalid radargram ID (uppercase) is a 400, not a 404 --
            // structurally invalid vs. legitimately absent (#118's
            // distinction, applied here to dataset lookup too).
            let (status, body) = get(&app, "/api/v1/datasets/Not-Valid").await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            let json: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(json["error"]["code"], "invalid_radargram_id");

            // Profiles.
            let (status, body) = get(&app, "/api/v1/profiles").await;
            assert_eq!(status, StatusCode::OK);
            let json: Value = serde_json::from_slice(&body).unwrap();
            assert!(json
                .as_array()
                .unwrap()
                .contains(&Value::String("default".into())));

            // Overview image.
            let (status, body) = get(
                &app,
                "/api/v1/datasets/route-test-a/views/standard/overview",
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            assert!(image::load_from_memory(&body).is_ok());

            // Unknown dataset view.
            let (status, body) =
                get(&app, "/api/v1/datasets/route-test-a/views/bogus/overview").await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            let json: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(json["error"]["code"], "unknown_dataset_view");

            // Unknown profile.
            let (status, _) = get(
                &app,
                "/api/v1/datasets/route-test-a/views/standard/overview?profile=nonexistent",
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST);

            // A valid chunk.
            let (status, body) = get(
                &app,
                "/api/v1/datasets/route-test-a/views/standard/chunks/default/0/0",
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            let decoded = image::load_from_memory(&body).unwrap();
            assert_eq!(decoded.width(), crate::render::grid::CHUNK_SIZE as u32);

            // Structurally invalid chunk coordinate (not a number) -> 400.
            let (status, body) = get(
                &app,
                "/api/v1/datasets/route-test-a/views/standard/chunks/default/abc/0",
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            let json: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(json["error"]["code"], "invalid_chunk_coordinate");

            // Well-formed but out-of-grid chunk coordinate -> 404, not 400.
            let (status, body) = get(
                &app,
                "/api/v1/datasets/route-test-a/views/standard/chunks/default/999/999",
            )
            .await;
            assert_eq!(status, StatusCode::NOT_FOUND);
            let json: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(json["error"]["code"], "image_chunk_not_found");

            // Pages: index and viewer.
            let (status, body) = get(&app, "/").await;
            assert_eq!(status, StatusCode::OK);
            let html = String::from_utf8(body.to_vec()).unwrap();
            assert!(html.contains("route-test-a"));

            let (status, body) = get(&app, "/view/route-test-a").await;
            assert_eq!(status, StatusCode::OK);
            let html = String::from_utf8(body.to_vec()).unwrap();
            assert!(html.contains("route-test-a"));

            let (status, _) = get(&app, "/view/does-not-exist").await;
            assert_eq!(status, StatusCode::NOT_FOUND);

            // Static assets are embedded, not proxied to a filesystem path.
            for asset in [
                "/static/leaflet.js",
                "/static/leaflet.css",
                "/static/app.css",
                "/static/app.js",
                "/static/index.js",
                "/static/viewer.js",
                "/static/images/logo.svg",
                "/favicon.ico",
            ] {
                let (status, body) = get(&app, asset).await;
                assert_eq!(status, StatusCode::OK, "{asset}");
                assert!(!body.is_empty(), "{asset}");
            }
        });
    }

    #[test]
    #[test_retry::retry]
    #[serial_test::serial(netcdf)]
    fn index_page_renders_lazy_overview_thumbnails() {
        // #121 requires an ~512px overview per catalog entry, and names
        // loading="lazy" as the mechanism bounding initial render work.
        // Both were missing when M7 was first reported complete, so they
        // are pinned here rather than left to visual inspection.
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let dir = tempfile::tempdir().unwrap();
            write_test_nc(&dir.path().join("a.nc"), "thumb-test-a");
            let app = test_app(dir.path());

            let (status, body) = get(&app, "/").await;
            assert_eq!(status, StatusCode::OK);
            let html = String::from_utf8(body.to_vec()).unwrap();

            assert!(
                html.contains("/api/v1/datasets/thumb-test-a/views/standard/overview"),
                "index must embed the overview image URL"
            );
            assert!(
                html.contains("loading=\"lazy\""),
                "overview images must be lazily loaded"
            );
            // The viewer's back-link targets /#card-{id}; the anchor has to
            // survive the table -> card-grid restructuring.
            assert!(
                html.contains("id=\"card-thumb-test-a\""),
                "per-entry anchor must be preserved"
            );
            assert!(
                html.contains("/static/app.css"),
                "first-party stylesheet must be linked"
            );
            assert!(
                html.contains("/static/images/logo.svg"),
                "logo must be shown beside the wordmark in the shared header"
            );
        });
    }

    #[test]
    #[test_retry::retry]
    #[serial_test::serial(netcdf)]
    fn index_page_render_profile_switcher_propagates_to_links() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let dir = tempfile::tempdir().unwrap();
            write_test_nc(&dir.path().join("a.nc"), "profile-test-a");
            let app = test_app(dir.path());

            let (status, body) = get(&app, "/?profile=positive").await;
            assert_eq!(status, StatusCode::OK);
            let html = String::from_utf8(body.to_vec()).unwrap();
            // The chosen profile propagates to both the thumbnail source
            // and the card's own link, so opening a radargram keeps the
            // profile the index was browsing in.
            assert!(html.contains(
                "/api/v1/datasets/profile-test-a/views/standard/overview?profile=positive"
            ));
            assert!(html.contains("/view/profile-test-a?profile=positive"));
            assert!(
                html.contains("value=\"positive\" selected"),
                "the switcher must reflect the active profile"
            );
            // Path/Processed are tucked behind "More info", not shown
            // directly in the scannable part of the card.
            assert!(html.contains("More info"));

            let (status, _) = get(&app, "/?profile=nonexistent").await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
        });
    }

    #[test]
    #[test_retry::retry]
    #[serial_test::serial(netcdf)]
    fn track_attributes_and_group_routes() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let dir = tempfile::tempdir().unwrap();
            write_test_nc_with_track(&dir.path().join("a.nc"), "track-a", Some("shared-group"));
            write_test_nc_with_track(&dir.path().join("b.nc"), "track-b", Some("shared-group"));
            write_test_nc_with_track(&dir.path().join("c.nc"), "track-c", None);
            let app = test_app(dir.path());

            // This radargram's own track.
            let (status, body) = get(&app, "/api/v1/datasets/track-a/track").await;
            assert_eq!(status, StatusCode::OK);
            let json: Value = serde_json::from_slice(&body).unwrap();
            let segments = json["segments"].as_array().unwrap();
            assert!(!segments.is_empty());
            let first_vertex = &segments[0]["vertices"][0];
            assert!(first_vertex["lon"].as_f64().is_some());
            assert!(first_vertex["lat"].as_f64().is_some());

            // Full raw attribute set, for the metadata dialog, plus the
            // curated display entries built from it.
            let (status, body) = get(&app, "/api/v1/datasets/track-a/attributes").await;
            assert_eq!(status, StatusCode::OK);
            let json: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(json["raw"]["ridal_radargram_id"], "track-a");
            assert_eq!(json["raw"]["crs"], "EPSG:32633");
            let entries = json["entries"].as_array().unwrap();
            assert!(
                entries
                    .iter()
                    .any(|e| e["label"] == "CRS" && e["value"] == "EPSG:32633"),
                "{entries:?}"
            );
            // The revision checksum is server-computed, never a file
            // attribute, so it must still show up as a curated entry.
            assert!(
                entries.iter().any(|e| e["label"] == "Revision"),
                "{entries:?}"
            );
            // The fixture writes no original_filepaths attribute; the
            // dedicated field must still be present (as an empty array),
            // not missing or an error.
            assert_eq!(json["original_filepaths"].as_array().unwrap().len(), 0);

            // The `/axes` route degrades each axis to null independently
            // when the fixture never wrote distance/twtt/depth.
            let (status, body) = get(&app, "/api/v1/datasets/track-a/axes").await;
            assert_eq!(status, StatusCode::OK);
            let json: Value = serde_json::from_slice(&body).unwrap();
            assert!(json["distance"].is_null());
            assert!(json["twtt"].is_null());
            assert!(json["depth"].is_null());

            // Group tracks: both group members present, the ungrouped one absent.
            let (status, body) = get(&app, "/api/v1/groups/shared-group/tracks").await;
            assert_eq!(status, StatusCode::OK);
            let json: Value = serde_json::from_slice(&body).unwrap();
            let obj = json.as_object().unwrap();
            assert!(obj.contains_key("track-a"));
            assert!(obj.contains_key("track-b"));
            assert!(!obj.contains_key("track-c"));
            assert!(obj["track-a"]["track"]["segments"].is_array());

            // An empty/unknown group is an empty object, not an error.
            let (status, body) = get(&app, "/api/v1/groups/no-such-group/tracks").await;
            assert_eq!(status, StatusCode::OK);
            let json: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(json.as_object().unwrap().len(), 0);

            // The reserved "no group" sentinel matches ungrouped entries
            // specifically -- not a literal group id -- so track-c (the
            // only ungrouped member of this catalog) is the one that
            // shows up here.
            let (status, body) = get(&app, &format!("/api/v1/groups/{NO_GROUP_ID}/tracks")).await;
            assert_eq!(status, StatusCode::OK);
            let json: Value = serde_json::from_slice(&body).unwrap();
            let obj = json.as_object().unwrap();
            assert!(obj.contains_key("track-c"));
            assert!(!obj.contains_key("track-a"));
            assert!(!obj.contains_key("track-b"));
        });
    }

    #[test]
    #[test_retry::retry]
    #[serial_test::serial(netcdf)]
    fn index_page_gives_ungrouped_entries_a_map_like_any_group() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let dir = tempfile::tempdir().unwrap();
            write_test_nc_with_track(&dir.path().join("a.nc"), "grouped-a", Some("Some Group"));
            write_test_nc_with_track(&dir.path().join("b.nc"), "ungrouped-b", None);
            let app = test_app(dir.path());

            let (status, body) = get(&app, "/").await;
            assert_eq!(status, StatusCode::OK);
            let html = String::from_utf8(body.to_vec()).unwrap();

            assert!(html.contains(">Ungrouped<"), "{html}");
            // Same map treatment as a named group: a group-map div keyed
            // by the reserved sentinel id, which entries_in_group and
            // group_tracks both already resolve to "no group".
            assert!(
                html.contains(&format!("data-group=\"{NO_GROUP_ID}\"")),
                "{html}"
            );

            let (status, body) = get(&app, &format!("/api/v1/groups/{NO_GROUP_ID}/tracks")).await;
            assert_eq!(status, StatusCode::OK);
            let json: Value = serde_json::from_slice(&body).unwrap();
            assert!(json.as_object().unwrap().contains_key("ungrouped-b"));
        });
    }

    #[test]
    #[test_retry::retry]
    #[serial_test::serial(netcdf)]
    fn index_page_shows_ungrouped_heading_even_when_it_is_the_only_section() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let dir = tempfile::tempdir().unwrap();
            write_test_nc(&dir.path().join("a.nc"), "solo-ungrouped");
            let app = test_app(dir.path());

            let (status, body) = get(&app, "/").await;
            assert_eq!(status, StatusCode::OK);
            let html = String::from_utf8(body.to_vec()).unwrap();
            assert!(
                html.contains(">Ungrouped<"),
                "heading must show even with no named groups present: {html}"
            );
        });
    }

    #[test]
    #[test_retry::retry]
    #[serial_test::serial(netcdf)]
    fn a_replaced_catalog_is_what_later_requests_see() {
        // The point of the whole change: the catalog a request works
        // against is read at request time, not fixed at startup.
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let dir = tempfile::tempdir().unwrap();
            write_test_nc(&dir.path().join("a.nc"), "first");
            let (app, state) = test_app_with_state(dir.path(), 1);

            let (status, body) = get(&app, "/api/v1/datasets").await;
            assert_eq!(status, StatusCode::OK);
            let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(body["entries"].as_array().unwrap().len(), 1);
            assert_eq!(body["entries"][0]["radargram_id"], "first");

            // A second radargram appears on disk after startup, which today
            // would need a restart to notice.
            write_test_nc(&dir.path().join("b.nc"), "second");
            let catalog = crate::server::catalog::Catalog::discover(&state.roots[0].path);
            assert_eq!(catalog.entries.len(), 2, "the rediscovery found both");
            let services = state.catalog().open_radargrams();
            state.replace_catalog(catalog, services);

            let (status, body) = get(&app, "/api/v1/datasets").await;
            assert_eq!(status, StatusCode::OK);
            let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
            let ids: Vec<&str> = body["entries"]
                .as_array()
                .unwrap()
                .iter()
                .map(|e| e["radargram_id"].as_str().unwrap())
                .collect();
            assert_eq!(ids, vec!["first", "second"]);
        });
    }

    #[test]
    #[test_retry::retry]
    #[serial_test::serial(netcdf)]
    fn one_snapshot_answers_for_both_the_entry_and_its_render_service() {
        // Two locks taken in a fixed order prevent deadlock, not tearing.
        // With one each, a reader could take the catalog from one
        // generation and the render service from the next -- rendering a
        // radargram at the wrong shape, or being told a freshly added one
        // is unavailable. One snapshot answers both, so the pair is always
        // from one generation.
        let dir = tempfile::tempdir().unwrap();
        write_test_nc(&dir.path().join("a.nc"), "first");
        let (_app, state) = test_app_with_state(dir.path(), 1);

        let before = state.catalog();
        assert!(before.find_entry("first").is_some());
        assert!(before.radargram("first").is_some());

        // A swap to a catalog that has an entry but no service for it,
        // which is what a torn read would synthesise out of two good
        // generations.
        state.replace_catalog(
            crate::server::catalog::Catalog::default(),
            std::collections::HashMap::new(),
        );

        // The old snapshot still agrees with itself.
        assert!(before.find_entry("first").is_some());
        assert!(before.radargram("first").is_some());
        // And so does the new one.
        let after = state.catalog();
        assert!(after.find_entry("first").is_none());
        assert!(after.radargram("first").is_none());
    }

    #[test]
    #[test_retry::retry]
    #[serial_test::serial(netcdf)]
    fn a_snapshot_taken_before_a_swap_stays_usable_after_it() {
        // What `Arc<Catalog>` buys over cloning the entries out: a handler
        // that took a snapshot keeps working against it, so a page cannot
        // be built half from one catalog and half from the next.
        let dir = tempfile::tempdir().unwrap();
        write_test_nc(&dir.path().join("a.nc"), "first");
        let (_app, state) = test_app_with_state(dir.path(), 1);

        let before = state.catalog();
        state.replace_catalog(
            crate::server::catalog::Catalog::default(),
            std::collections::HashMap::new(),
        );

        assert_eq!(before.entries.len(), 1, "the old snapshot is intact");
        assert!(state.catalog().entries.is_empty(), "the new one is empty");
    }

    #[test]
    #[test_retry::retry]
    #[serial_test::serial(netcdf)]
    fn concurrent_requests_for_one_chunk_render_it_only_once() {
        // #119 requires that concurrent requests generate an item only
        // once. Nothing implements that explicitly -- it falls out of the
        // per-radargram Mutex plus the cache re-check at the top of
        // get_or_render_chunk: whichever request wins the lock renders and
        // inserts, and the ones queued behind it find the result already
        // cached. This pins that property so a future change to the
        // locking cannot quietly reintroduce duplicate rendering.
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let dir = tempfile::tempdir().unwrap();
            write_test_nc(&dir.path().join("a.nc"), "dup-test");
            let (app, state) = test_app_with_state(dir.path(), 8);

            let uri = "/api/v1/datasets/dup-test/views/standard/chunks/default/0/0";
            let mut handles = Vec::new();
            for _ in 0..8 {
                let app = app.clone();
                handles.push(tokio::spawn(async move { get(&app, uri).await }));
            }
            let mut responses = Vec::new();
            for handle in handles {
                responses.push(handle.await.unwrap());
            }

            let first = responses[0].1.clone();
            for (status, body) in &responses {
                assert_eq!(*status, StatusCode::OK);
                assert_eq!(body, &first, "concurrent renders disagreed");
            }

            let radargram = state.catalog().radargram("dup-test").unwrap();
            let service = radargram.service.lock().unwrap();
            assert_eq!(
                service.cache_len(),
                1,
                "the same chunk was rendered and cached more than once"
            );
        });
    }

    #[test]
    #[test_retry::retry]
    #[serial_test::serial(netcdf)]
    fn a_single_render_worker_still_serves_every_request() {
        // The permit semaphore is sized from --n-workers; at 1 it fully
        // serialises rendering. Everything must still be served (just
        // slower) rather than deadlocking or timing out into a 503.
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let dir = tempfile::tempdir().unwrap();
            write_test_nc(&dir.path().join("a.nc"), "one-worker");
            let (app, _state) = test_app_with_state(dir.path(), 1);

            let uris = [
                "/api/v1/datasets/one-worker/views/standard/chunks/default/0/0",
                "/api/v1/datasets/one-worker/views/standard/chunks/default/1/0",
                "/api/v1/datasets/one-worker/views/standard/overview",
            ];
            let mut handles = Vec::new();
            for uri in uris {
                let app = app.clone();
                handles.push(tokio::spawn(async move { get(&app, uri).await }));
            }
            for handle in handles {
                let (status, body) = handle.await.unwrap();
                assert_eq!(status, StatusCode::OK);
                assert!(!body.is_empty());
            }
        });
    }

    #[test]
    #[test_retry::retry]
    #[serial_test::serial(netcdf)]
    fn one_unreadable_file_does_not_break_the_whole_catalog_at_startup() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join("garbage.nc"), b"not a netcdf file").unwrap();
            write_test_nc(&dir.path().join("good.nc"), "still-works");

            let app = test_app(dir.path());
            let (status, body) = get(&app, "/api/v1/datasets").await;
            assert_eq!(status, StatusCode::OK);
            let json: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(json["entries"].as_array().unwrap().len(), 1);
            assert!(!json["warnings"].as_array().unwrap().is_empty());

            let (status, _) =
                get(&app, "/api/v1/datasets/still-works/views/standard/overview").await;
            assert_eq!(status, StatusCode::OK);
        });
    }
}
