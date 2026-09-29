//! The site server (#214): one entry point, many projects.
//!
//! [`SiteState`] holds the site's registry, its accounts and one shared
//! render budget. Each project is served by its own
//! [`build_router`](super::app::build_router), which knows nothing of keys or
//! accounts: a request to `/p/{key}/…` or `/api/v1/projects/{key}/…` is
//! rewritten to the project-relative path and dispatched in-process with
//! [`tower::ServiceExt::oneshot`].
//!
//! `ridal gui` is served the same way, as a site of one: its project is
//! `default`, at `/p/default/`, and its host is [`Host::Local`] rather than a
//! site directory, so there are no accounts and nothing to manage.
//!
//! # Identity
//!
//! Site accounts live in `accounts.json` at the site root; a project holds
//! only memberships (`users.json`). [`site_middleware`] resolves the account
//! behind a request once, as a [`SiteCaller`], for the site's own routes.
//! A request for a project gets its project [`Caller`](super::auth::Caller)
//! from the same resolution plus that project's memberships
//! ([`auth::project_caller`]), handed to the project router in the request
//! extensions, so no project route reads an account or a cookie itself.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

use axum::extract::{FromRequestParts, Path, Request, State};
use axum::http::{header, request::Parts, HeaderMap, StatusCode, Uri};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use tokio::sync::Semaphore;
use tower::ServiceExt;

use super::app::{AccessOptions, AppState, SiteContext};
use super::auth::{self, SessionKey, SESSION_TTL_DAYS};
use super::render_service::RenderServiceConfig;
use super::routes::{ApiError, PageError};
use super::templates;
use crate::identity::{ProjectKey, UserId};
use crate::project::members;
use crate::project::roles::{DownloadScope, Role};
use crate::project::store::{DocumentStore, Expectation};
use crate::site::accounts::{self, bulk, invite, Account, AccountError, AccountSet};
use crate::site::{audit as site_audit, Grant, Site, SiteError};

/// The site's account file, for the "create the first one" hint.
const ACCOUNTS_FILE: &str = accounts::ACCOUNTS_FILE;

/// A project's built state and router, cached until it is evicted.
///
/// Built lazily on the first request that names the key, so starting a site
/// with a hundred projects opens none of them until someone looks.
pub struct ProjectRuntime {
    pub state: Arc<AppState>,
    pub router: Router,
}

/// What a [`SiteState`] serves.
pub enum Host {
    /// `ridal server start`: a site directory of projects, with accounts.
    Site(Site),
    /// `ridal gui`: one directory, served as the project `default` to the one
    /// person at this machine. No accounts, and no site to manage.
    Local {
        /// What the browser tab calls it.
        name: String,
        /// Where that person's settings live: the project's own data
        /// directory, the same place its project preferences are kept.
        /// `None` for a bare directory of radargrams, which has nowhere.
        store: Option<DocumentStore>,
    },
}

/// Everything the site server needs that is not per-project.
pub struct SiteState {
    pub host: Host,
    /// What this server permits, independent of who is asking. Copied into
    /// every project's [`AppState`], so `--read-only` caps every one.
    pub access: AccessOptions,
    /// Kept to build a project's render services when it is first opened.
    render_config: RenderServiceConfig,
    /// One render budget for the whole site, so `--n-workers` means
    /// "concurrent renders across this server" rather than per project.
    pub render_permits: Arc<Semaphore>,
    /// The projects opened so far, keyed by their immutable key.
    projects: RwLock<HashMap<ProjectKey, Arc<ProjectRuntime>>>,
    /// The site-wide cookie-signing key, loaded on first use.
    session_key: Mutex<Option<SessionKey>>,
}

impl SiteState {
    pub fn new(site: Site, access: AccessOptions, render_config: RenderServiceConfig) -> Arc<Self> {
        Arc::new(Self {
            host: Host::Site(site),
            access,
            // `.max(1)`: a zero-permit semaphore would deadlock every render.
            render_permits: Arc::new(Semaphore::new(render_config.n_workers.max(1))),
            render_config,
            projects: RwLock::new(HashMap::new()),
            session_key: Mutex::new(None),
        })
    }

    /// `ridal gui`'s site of one: `state`, already built, as the project
    /// `default` (#214).
    pub fn local(state: AppState) -> Arc<Self> {
        let name = state
            .project
            .as_ref()
            .and_then(|project| project.config().project.name)
            .unwrap_or_else(|| "Ridal".to_string());
        let store = state
            .project
            .as_ref()
            .map(|project| DocumentStore::new(project.documents().root().to_path_buf()));
        let access = state.access;
        let render_permits = Arc::clone(&state.render_permits);
        let state = Arc::new(state);
        let runtime = Arc::new(ProjectRuntime {
            router: super::app::build_router(Arc::clone(&state)),
            state,
        });
        let key = local_key();
        Arc::new(Self {
            host: Host::Local { name, store },
            access,
            render_permits,
            render_config: runtime.state.render_config(),
            projects: RwLock::new(HashMap::from([(key, runtime)])),
            session_key: Mutex::new(None),
        })
    }

    /// The site directory, or a 404 under `ridal gui`, which serves one
    /// project and has no site to manage.
    pub fn site(&self) -> Result<&Site, ApiError> {
        match &self.host {
            Host::Site(site) => Ok(site),
            Host::Local { .. } => Err(ApiError::not_found(
                "no_site",
                "`ridal gui` serves a single project; there is no site to manage.",
            )),
        }
    }

    /// Refuse unless the caller is a server administrator. Under `ridal
    /// gui` there is no site to administer, which is a 404 before it is a
    /// question of who is asking.
    fn require_server_admin(&self, caller: &SiteCaller, action: &str) -> Result<(), ApiError> {
        self.site()?;
        caller.require_server_admin(action)
    }

    /// The name shown in the browser.
    pub fn name(&self) -> String {
        match &self.host {
            Host::Site(site) => site.name(),
            Host::Local { name, .. } => name.clone(),
        }
    }

    /// Whether anyone can sign in here. Never under `ridal gui`.
    fn accounts_configured(&self) -> bool {
        match &self.host {
            Host::Site(site) => accounts::is_configured(site.store()).unwrap_or(false),
            Host::Local { .. } => false,
        }
    }

    /// Where a person's site-wide settings are kept: the site root, or the
    /// project's own data directory under `ridal gui`.
    fn preferences_store(&self) -> Option<&DocumentStore> {
        match &self.host {
            Host::Site(site) => Some(site.store()),
            Host::Local { store, .. } => store.as_ref(),
        }
    }

    /// The site-wide signing key, loaded from the site root on first use.
    pub fn session_key(&self) -> Result<SessionKey, String> {
        let mut guard = self
            .session_key
            .lock()
            .map_err(|_| "the session key lock was poisoned by a panic".to_string())?;
        if let Some(key) = guard.as_ref() {
            return Ok(key.clone());
        }
        let key = SessionKey::load_or_create(
            self.site()
                .map_err(|_| "`ridal gui` has no sessions".to_string())?
                .store(),
        )?;
        *guard = Some(key.clone());
        Ok(key)
    }

    /// The runtime for `key`, if it has been built already.
    fn cached(&self, key: &ProjectKey) -> Option<Arc<ProjectRuntime>> {
        self.projects.read().ok()?.get(key).map(Arc::clone)
    }

    /// The runtime for `key`, building and caching it on first use.
    pub fn runtime(&self, key: &ProjectKey) -> Result<Arc<ProjectRuntime>, ApiError> {
        if let Some(runtime) = self.cached(key) {
            return Ok(runtime);
        }

        // Under `ridal gui` the one project is built at startup, so a miss is
        // a key that does not exist.
        let site = self.site().map_err(|_| hidden_project())?;
        let project = site.project(key).map_err(site_error)?;
        let root = project.root().to_path_buf();
        let state =
            AppState::build_with_project(&root, &self.render_config, Some(project), self.access)
                .map_err(|e| ApiError::internal("project_open_failed", e))?
                .with_site(
                    key.clone(),
                    Arc::new(SiteContext {
                        store: DocumentStore::new(site.root().to_path_buf()),
                        archived: site.is_archived(key),
                    }),
                )
                .with_render_permits(Arc::clone(&self.render_permits));
        let state = Arc::new(state);
        let router = super::app::build_router(Arc::clone(&state));
        let runtime = Arc::new(ProjectRuntime { state, router });

        let mut cache = self
            .projects
            .write()
            .map_err(|_| ApiError::internal("lock_failed", "the project cache was poisoned"))?;
        Ok(Arc::clone(cache.entry(key.clone()).or_insert(runtime)))
    }

    /// Refuse a change to the site on a server started `--read-only`.
    ///
    /// `--read-only` caps every project's callers at viewer; this is the same
    /// promise for the site's own routes, which have no project `Caller` to
    /// carry the cap. Checked after the caller's standing, so an anonymous
    /// caller is still asked to sign in rather than told about the server.
    fn require_writable(&self, action: &str) -> Result<(), ApiError> {
        if self.access.read_only {
            return Err(ApiError::forbidden(
                "read_only",
                format!("This server was started read-only, so you cannot {action}."),
            ));
        }
        Ok(())
    }

    /// [`Self::require_writable`], and refuse too when the project is
    /// archived: an archived project is read-only for its members, access
    /// policy and invitations as much as for its catalog.
    fn require_project_writable(&self, key: &ProjectKey, action: &str) -> Result<(), ApiError> {
        self.require_writable(action)?;
        if self.site()?.is_archived(key) {
            return Err(ApiError::forbidden(
                "archived",
                format!(
                    "This project is archived, so you cannot {action}. Unarchive it \
                     first."
                ),
            ));
        }
        Ok(())
    }

    /// Drop a cached project, so the next request rebuilds it. Used after an
    /// archive change, a rename or a delete.
    fn evict(&self, key: &ProjectKey) {
        if let Ok(mut cache) = self.projects.write() {
            cache.remove(key);
        }
    }
}

/// The refusal for a project the caller may not see, and for one that does
/// not exist: the same words, so the two cannot be told apart.
fn hidden_project() -> ApiError {
    ApiError::not_found(
        "project_not_found",
        "No such project, or you are not a member of it.",
    )
}

/// A site error as an HTTP answer.
///
/// Anything but the expected outcomes is logged in full and answered in
/// general terms: those messages name paths on the server, which are not
/// the caller's business.
fn site_error(error: SiteError) -> ApiError {
    match error {
        SiteError::NotFound(_) => hidden_project(),
        SiteError::KeyInUse(_) => ApiError::conflict("project_exists", error.to_string()),
        SiteError::NotArchived(_) => ApiError::conflict("not_archived", error.to_string()),
        SiteError::Account(error) => account_error(error),
        other => {
            eprintln!("Warning: {other}");
            ApiError::internal(
                "project_unavailable",
                "This project cannot be opened. The server's log says why.",
            )
        }
    }
}

/// The account name behind a request, for the site audit. These routes all
/// require a session, so `anonymous` should not be reached.
fn actor(caller: &SiteCaller) -> String {
    caller
        .user
        .as_ref()
        .map(|user| user.as_str().to_string())
        .unwrap_or_else(|| "anonymous".to_string())
}

/// Append one site-audit entry. Never fails the change it describes, for the
/// same reason the project audit does not: the change has already happened.
fn audit(site: &SiteState, entry: site_audit::Entry) {
    if let Ok(directory) = site.site() {
        site_audit::record(directory.store(), entry);
    }
}

/// Resolve an optional project key from a request body, refusing one that
/// does not exist. Shared by account creation and the bulk routes, so a typo
/// is reported the same way everywhere.
fn resolve_optional_project(
    site: &SiteState,
    raw: Option<&str>,
) -> Result<Option<ProjectKey>, ApiError> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let key = parse_key(raw)?;
    if !site.site()?.project_path(&key).is_dir() {
        return Err(ApiError::not_found(
            "project_not_found",
            format!("No project '{key}' in this site."),
        ));
    }
    site.require_project_writable(&key, "invite people into it")?;
    Ok(Some(key))
}

/// A project key from a path, refused in the usual envelope.
fn parse_key(raw: &str) -> Result<ProjectKey, ApiError> {
    ProjectKey::new(raw).map_err(|e| ApiError::bad_request("invalid_project_key", e))
}

/// An account name from a path or a body, refused in the usual envelope.
fn parse_name(raw: &str, code: &'static str) -> Result<UserId, ApiError> {
    UserId::new(raw).map_err(|e| ApiError::bad_request(code, e))
}

/// The site's accounts, or none at all when it has no account file yet.
fn account_set(site: &SiteState) -> Result<AccountSet, ApiError> {
    Ok(accounts::read(site.site()?.store())
        .map_err(account_error)?
        .map(|(set, _)| set)
        .unwrap_or_default())
}

fn account_error(error: AccountError) -> ApiError {
    match &error {
        AccountError::Store(e) => ApiError::internal("store_failed", e.to_string()),
        AccountError::Malformed { .. } => {
            ApiError::internal("malformed_accounts", error.to_string())
        }
        AccountError::Duplicate(_) => ApiError::conflict("account_exists", error.to_string()),
        AccountError::NotFound(_) => ApiError::not_found("account_not_found", error.to_string()),
        AccountError::Rejected(_) => ApiError::bad_request("rejected", error.to_string()),
        AccountError::Hash(_) => ApiError::internal("hash_failed", error.to_string()),
    }
}

fn member_error(error: members::MemberError) -> ApiError {
    match &error {
        members::MemberError::Store(e) => ApiError::internal("store_failed", e.to_string()),
        members::MemberError::Malformed { .. } => {
            ApiError::internal("malformed_members", error.to_string())
        }
        members::MemberError::NotFound(_) => {
            ApiError::not_found("member_not_found", error.to_string())
        }
        members::MemberError::Rejected(_) => ApiError::bad_request("rejected", error.to_string()),
    }
}

/// Who is asking, at the site level.
///
/// Resolved once per request by [`site_middleware`] and read from the
/// request extensions. Distinct from [`super::auth::Caller`], which is about
/// a *project*: a server administrator's standing does not depend on which
/// project a request targets, and the landing and account routes have no
/// project at all.
#[derive(Debug, Clone)]
pub struct SiteCaller {
    pub user: Option<UserId>,
    pub server_admin: bool,
    /// Whether the site has an account file at all.
    pub accounts_configured: bool,
}

impl SiteCaller {
    fn identity(&self) -> auth::SiteIdentity<'_> {
        auth::SiteIdentity {
            user: self.user.as_ref(),
            server_admin: self.server_admin,
            accounts_configured: self.accounts_configured,
        }
    }

    fn require_server_admin(&self, action: &str) -> Result<(), ApiError> {
        if self.server_admin {
            return Ok(());
        }
        if self.user.is_none() {
            return Err(ApiError::unauthorized(
                "authentication_required",
                format!("Sign in as a server administrator to {action}."),
            ));
        }
        Err(ApiError::forbidden(
            "insufficient_role",
            format!("Only a server administrator may {action}."),
        ))
    }
}

impl FromRequestParts<Arc<SiteState>> for SiteCaller {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        _state: &Arc<SiteState>,
    ) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<SiteCaller>()
            .cloned()
            .ok_or_else(|| {
                ApiError::internal(
                    "caller_unresolved",
                    "The request reached a handler without an identity. This is a bug \
                 in how the router was assembled.",
                )
            })
    }
}

/// Resolve a site caller from the session cookie and `accounts.json`.
///
/// A damaged account file fails closed: no account matches, so only an
/// anonymous caller remains. Under `ridal gui` there are no accounts, and
/// the one person is the local default user wherever there is a project to
/// keep their settings in.
fn resolve_caller(site: &SiteState, headers: &HeaderMap, now: i64) -> SiteCaller {
    let directory = match &site.host {
        Host::Site(directory) => directory,
        Host::Local { store, .. } => {
            return SiteCaller {
                user: store
                    .as_ref()
                    .and_then(|_| UserId::new(crate::identity::DEFAULT_USER).ok()),
                server_admin: false,
                accounts_configured: false,
            }
        }
    };
    let accounts = match accounts::read(directory.store()) {
        Ok(Some((set, _))) => Some(set),
        Ok(None) => None,
        Err(_) => Some(AccountSet::default()),
    };
    let account = accounts.as_ref().and_then(|set| {
        let key = site.session_key().ok()?;
        auth::signed_in(set, &key, headers, now)
    });
    SiteCaller {
        user: account.map(|account| account.name.clone()),
        server_admin: account.is_some_and(|account| account.server_admin),
        accounts_configured: accounts.is_some(),
    }
}

async fn site_middleware(
    State(site): State<Arc<SiteState>>,
    mut request: Request,
    next: axum::middleware::Next,
) -> Response {
    let caller = resolve_caller(&site, request.headers(), auth::now());
    request.extensions_mut().insert(caller);
    next.run(request).await
}

/// Build the site's Axum application.
pub fn build_site_router(site: Arc<SiteState>) -> Router {
    Router::new()
        .route("/", get(landing))
        .route("/login", get(login_page))
        .route("/invite/{token}", get(invite_page))
        // The site's own settings: the per-user, site-wide preferences and
        // (for a server administrator) the accounts. Project settings stay
        // under the project's key.
        .route("/settings", get(site_settings_page))
        .route("/api/v1/health", get(super::routes::health))
        .route("/api/v1/site", get(site_info))
        .route("/api/v1/site/audit", get(site_audit_log))
        .route("/api/v1/site/memberships", get(site_memberships))
        .route(
            "/api/v1/site/preferences",
            get(get_site_preferences).put(put_site_preferences),
        )
        .route("/api/v1/projects", get(list_projects).post(create_project))
        .route(
            "/api/v1/projects/{key}",
            get(project_info)
                .patch(update_project)
                .delete(delete_project),
        )
        .route("/api/v1/projects/{key}/archive", post(archive_project))
        .route("/api/v1/projects/{key}/unarchive", post(unarchive_project))
        // Membership and access are project-scoped but administered through
        // the site, because they name site accounts. Registered explicitly so
        // they win over the fallback that delegates everything else.
        .route(
            "/api/v1/projects/{key}/members",
            get(list_members).post(add_member),
        )
        .route(
            "/api/v1/projects/{key}/members/{name}",
            put(update_member).delete(remove_member),
        )
        // A project administrator may create an account for their own
        // project, but only through this route: it can grant nothing beyond
        // the project in its path. Account lifecycle stays at the site
        // level, for a server administrator.
        .route("/api/v1/projects/{key}/members/invite", post(invite_member))
        // A project administrator's bulk creation, likewise scoped to the
        // project in the path.
        .route(
            "/api/v1/projects/{key}/members/bulk/invites",
            post(create_project_bulk_invites),
        )
        .route(
            "/api/v1/projects/{key}/members/bulk/passwords",
            post(create_project_bulk_passwords),
        )
        .route("/api/v1/projects/{key}/audit", get(project_audit_log))
        .route("/api/v1/projects/{key}/access", put(put_project_access))
        .route("/api/v1/accounts", get(list_accounts).post(create_account))
        .route(
            "/api/v1/accounts/{name}",
            put(update_account).delete(delete_account),
        )
        .route(
            "/api/v1/accounts/{name}/invite",
            post(reissue_account_invite),
        )
        // Bulk creation mirrors the project endpoints that preceded it
        // (#214): a batch of one-time invite links, or accounts with
        // generated passwords for a workshop. Registered before the `{name}`
        // routes would not matter -- matchit prefers a literal segment -- but
        // the two-segment paths cannot collide with a single name anyway.
        .route("/api/v1/accounts/bulk/invites", post(create_bulk_invites))
        .route(
            "/api/v1/accounts/bulk/passwords",
            post(create_bulk_passwords),
        )
        .route("/api/v1/auth/me", get(me))
        .route("/api/v1/auth/login", post(login))
        .route("/api/v1/auth/logout", post(logout))
        .route("/api/v1/auth/invite", post(redeem_invite))
        // Everything else that looks like a project is that project's own
        // router. The layer resolves identity for the site routes above and
        // for this fallback alike.
        .fallback(project_fallback)
        .layer(axum::middleware::from_fn_with_state(
            Arc::clone(&site),
            site_middleware,
        ))
        .with_state(site)
        // Stateless, and merged last so assets are served without touching
        // `accounts.json`.
        .merge(super::app::static_router())
}

// ---------------------------------------------------------------------------
// Delegating to a project
// ---------------------------------------------------------------------------

/// The key `ridal gui` serves its project under.
fn local_key() -> ProjectKey {
    ProjectKey::new(super::app::DEFAULT_PROJECT_KEY).expect("the default key is a valid slug")
}

/// A page of `ridal gui`'s project, from its project-relative path.
fn local_page(path: &str) -> String {
    format!("/p/{}{path}", super::app::DEFAULT_PROJECT_KEY)
}

/// Rewrite a site path to the project-relative path its router expects.
///
/// `/p/{key}/view/x` becomes `/view/x`; `/api/v1/projects/{key}/datasets`
/// becomes `/api/v1/datasets`.
fn split_project_path(path: &str) -> Option<(ProjectKey, String)> {
    let api = path.starts_with("/api/v1/projects/");
    let rest = if api {
        path.strip_prefix("/api/v1/projects/")?
    } else {
        path.strip_prefix("/p/")?
    };
    let (key, tail) = match rest.split_once('/') {
        Some((key, tail)) => (key, format!("/{tail}")),
        None => (rest, String::new()),
    };
    let key = ProjectKey::new(key).ok()?;
    let rewritten = if api {
        format!("/api/v1{tail}")
    } else if tail.is_empty() {
        "/".to_string()
    } else {
        tail
    };
    Some((key, rewritten))
}

/// Hand a request for a project to that project's router, with the caller
/// the site resolved for it.
async fn project_fallback(State(site): State<Arc<SiteState>>, mut request: Request) -> Response {
    let path = request.uri().path().to_string();
    let Some((key, rewritten)) = split_project_path(&path) else {
        return ApiError::not_found("not_found", "No such page.").into_response();
    };
    let refuse = |error: ApiError| {
        if rewritten.starts_with("/api/") {
            error.into_response()
        } else {
            PageError(error).into_response()
        }
    };
    let Some(identity) = request.extensions().get::<SiteCaller>().cloned() else {
        return refuse(ApiError::internal(
            "caller_unresolved",
            "The request reached a project without an identity.",
        ));
    };
    // Building a project opens every radargram in it, so a caller who may
    // not see the project must be turned away before that, not by the
    // project's own middleware after it. Once built, that middleware is
    // enough: it answers the same 404 without the cost.
    // A project the caller may not see and one that does not exist get the
    // same answer: a request to sign in for someone who is not, a 404 for
    // someone who is. Matching the project's own middleware (see
    // `auth::middleware`), so it makes no difference whether it was built.
    let unseen = || {
        if identity.user.is_none() && identity.accounts_configured {
            auth::login_required(&rewritten)
        } else {
            auth::hidden_project(&rewritten)
        }
    };
    let runtime = match site.cached(&key) {
        Some(runtime) => runtime,
        None => {
            match standing(&site, &key, &identity) {
                Ok((_, _, Standing::Hidden)) => return unseen(),
                Ok(_) => {}
                Err(_)
                    if !site
                        .site()
                        .is_ok_and(|directory| directory.project_path(&key).is_dir()) =>
                {
                    return unseen()
                }
                Err(error) => return refuse(error),
            }
            match site.runtime(&key) {
                Ok(runtime) => runtime,
                Err(error) => return refuse(error),
            }
        }
    };
    let state = &runtime.state;
    let caller = match &site.host {
        Host::Local { .. } => auth::local_caller(state.project.is_some(), site.access.read_only),
        Host::Site(_) => auth::project_caller(
            &identity.identity(),
            state.project.as_ref(),
            state.site.as_ref().is_some_and(|context| context.archived),
            site.access.read_only,
        ),
    };
    request.extensions_mut().insert(caller);

    let mut parts = request.into_parts();
    let query = parts
        .0
        .uri
        .query()
        .map(|query| format!("?{query}"))
        .unwrap_or_default();
    match format!("{rewritten}{query}").parse::<Uri>() {
        Ok(uri) => parts.0.uri = uri,
        Err(_) => {
            return ApiError::bad_request("invalid_path", "The path could not be rewritten.")
                .into_response()
        }
    }
    let request = Request::from_parts(parts.0, parts.1);
    match runtime.router.clone().oneshot(request).await {
        Ok(response) => response,
        Err(infallible) => match infallible {},
    }
}

// ---------------------------------------------------------------------------
// Site pages
// ---------------------------------------------------------------------------

fn render(template: &str, context: minijinja::Value) -> Result<Html<String>, PageError> {
    let env = templates::environment();
    let tmpl = env
        .get_template(template)
        .expect("the template is always registered");
    tmpl.render(context)
        .map(Html)
        .map_err(|e| PageError(ApiError::internal("template_error", e.to_string())))
}

/// A site page's base URLs: no project, so the project-scoped base is the
/// site API itself and project pages are addressed by key in the body.
fn site_page_bases() -> (String, String, String) {
    ("/api/v1".to_string(), "/api/v1".to_string(), String::new())
}

/// The theme a site page renders with (#214), or `""` to follow the device.
///
/// Site settings are per account and site-wide: the same value applies to
/// every project, which is what makes the theme a *site* setting rather than
/// one saved separately for each project.
fn site_theme(site: &SiteState, user: Option<&UserId>) -> String {
    let (Some(user), Some(store)) = (user, site.preferences_store()) else {
        return String::new();
    };
    crate::project::preferences::read_lenient(store, user)
        .theme
        .filter(|theme| super::routes::is_offered_theme(theme))
        .unwrap_or_default()
}

/// `GET /` -- the landing page.
///
/// A site that has accounts asks for a login first; one that does not (a
/// read-only public catalog) lists nothing, since its projects are opened by
/// their links. `ridal gui` has one project, so it goes straight there.
async fn landing(
    State(site): State<Arc<SiteState>>,
    caller: SiteCaller,
) -> Result<Response, PageError> {
    let directory = match &site.host {
        Host::Site(directory) => directory,
        Host::Local { .. } => return Ok(Redirect::to(&local_page("/")).into_response()),
    };
    let configured = site.accounts_configured();
    if configured && caller.user.is_none() {
        return Ok(Redirect::to("/login").into_response());
    }

    let keys = directory
        .list()
        .map_err(|e| PageError(ApiError::internal("site_error", e.to_string())))?;
    let projects: Vec<serde_json::Value> = keys
        .iter()
        .filter_map(|key| project_entry(&site, key, &caller, Lookup::Listing))
        .collect();

    let (api_base, site_api_base, page_base) = site_page_bases();
    let html = render(
        "site.html.jinja",
        minijinja::context! {
            site_name => site.name(),
            projects => projects,
            project_count => keys.len(),
            current_user => caller.user.as_ref().map(|u| u.as_str()),
            current_role => if caller.server_admin { "admin" } else { "viewer" },
            server_admin => caller.server_admin,
            authentication_configured => configured,
            active_theme => site_theme(&site, caller.user.as_ref()),
            api_base => api_base,
            site_api_base => site_api_base,
            page_base => page_base,
        },
    )?;
    Ok(html.into_response())
}

/// Whether a project is being listed or looked up by its key.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Lookup {
    /// The landing and `GET /api/v1/projects`: only the caller's own
    /// projects, or every one for a server administrator. A public project
    /// is unlisted -- reachable by its link, never shown to a non-member.
    Listing,
    /// `GET /api/v1/projects/{key}`: whatever the caller may open.
    ByKey,
}

/// One project as the landing list and the API describe it, or `None` when
/// the caller may not see it.
fn project_entry(
    site: &SiteState,
    key: &ProjectKey,
    caller: &SiteCaller,
    lookup: Lookup,
) -> Option<serde_json::Value> {
    let (project, members, standing) = standing(site, key, caller).ok()?;
    match standing {
        Standing::Hidden => return None,
        Standing::Public if lookup == Lookup::Listing => return None,
        _ => {}
    }
    let member = caller.user.as_ref().and_then(|name| members.get(name));
    let (role, download) = match member {
        Some(member) => (member.role, member.download),
        None if caller.server_admin => (Role::Admin, DownloadScope::All),
        None => (Role::Viewer, members.anonymous_download),
    };
    // The catalog size, from the summary a scan leaves behind (#214). A
    // project that has never been scanned has none, and the card says so.
    let radargram_count = crate::project::catalog_summary::read(project.documents())
        .map(|summary| summary.radargrams);
    Some(serde_json::json!({
        "key": key.as_str(),
        "name": project
            .config()
            .project
            .name
            .unwrap_or_else(|| key.as_str().to_string()),
        "archived": site.site().ok()?.is_archived(key),
        "created_by": project.config().project.created_by.map(|user| user.as_str().to_string()),
        "member": member.is_some() || caller.server_admin,
        "member_count": members.members.len(),
        "radargram_count": radargram_count,
        "role": role.as_str(),
        "download": download.as_str(),
        "require_auth_to_read": members.require_auth_to_read,
    }))
}

/// `GET /settings` -- the site's own settings page.
///
/// Personal, site-wide settings (the theme) for anyone signed in, and the
/// accounts for a server administrator. A site with no accounts has nothing
/// to sign in to, so it is sent back to the landing, which explains that.
/// `ridal gui` keeps everything on its project's settings page.
async fn site_settings_page(
    State(site): State<Arc<SiteState>>,
    caller: SiteCaller,
) -> Result<Response, PageError> {
    let directory = match &site.host {
        Host::Site(directory) => directory,
        Host::Local { .. } => return Ok(Redirect::to(&local_page("/settings")).into_response()),
    };
    let configured = site.accounts_configured();
    if caller.user.is_none() {
        return Ok(Redirect::to(if configured { "/login" } else { "/" }).into_response());
    }

    let keys = directory
        .list()
        .map_err(|e| PageError(ApiError::internal("site_error", e.to_string())))?;
    let projects: Vec<serde_json::Value> = keys
        .iter()
        .filter_map(|key| project_entry(&site, key, &caller, Lookup::Listing))
        .collect();

    let (api_base, site_api_base, page_base) = site_page_bases();
    let html = render(
        "site_settings.html.jinja",
        minijinja::context! {
            site_name => site.name(),
            projects => projects,
            roles => Role::ALL.map(Role::as_str),
            download_scopes => DownloadScope::ALL.map(DownloadScope::as_str),
            min_password_len => accounts::MIN_PASSWORD_LEN,
            invite_ttl_days => invite::INVITE_TTL_DAYS,
            current_user => caller.user.as_ref().map(|u| u.as_str()),
            current_role => if caller.server_admin { "admin" } else { "viewer" },
            server_admin => caller.server_admin,
            authentication_configured => configured,
            active_theme => site_theme(&site, caller.user.as_ref()),
            api_base => api_base,
            site_api_base => site_api_base,
            page_base => page_base,
        },
    )?;
    Ok(html.into_response())
}

/// `GET /login`
async fn login_page(
    State(site): State<Arc<SiteState>>,
    caller: SiteCaller,
) -> Result<Html<String>, PageError> {
    let configured = site.accounts_configured();
    let (api_base, site_api_base, page_base) = site_page_bases();
    render(
        "login.html.jinja",
        minijinja::context! {
            authentication_configured => configured,
            already_signed_in => caller.user.is_some(),
            current_user => caller.user.as_ref().map(|u| u.as_str()),
            current_role => if caller.server_admin { "admin" } else { "viewer" },
            // Not a bare catalog: a site always has somewhere to save.
            project => true,
            // The way out as much as the way in, so a person who chose dark
            // does not get one light screen on the way past it (#141).
            active_theme => site_theme(&site, caller.user.as_ref()),
            api_base => api_base,
            site_api_base => site_api_base,
            page_base => page_base,
        },
    )
}

/// `GET /invite/{token}`
///
/// Says nothing about whether the token is good, so a link found in an old
/// log does not confirm an account exists.
async fn invite_page(Path(token): Path<String>) -> Result<Html<String>, PageError> {
    let (api_base, site_api_base, page_base) = site_page_bases();
    render(
        "invite.html.jinja",
        minijinja::context! {
            token => token,
            min_password_len => accounts::MIN_PASSWORD_LEN,
            api_base => api_base,
            site_api_base => site_api_base,
            page_base => page_base,
        },
    )
}

// ---------------------------------------------------------------------------
// Site identity API
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize)]
pub struct LoginBody {
    name: String,
    password: String,
}

fn password_login_allowed(site: &SiteState) -> Result<(), ApiError> {
    if site.access.allow_password_login {
        return Ok(());
    }
    Err(ApiError::forbidden(
        "insecure_transport",
        "This server is bound to a network address and does not terminate TLS, \
         so a password sent to it would travel in the clear. Reach it through a \
         TLS-terminating reverse proxy, or restart it with --allow-insecure-login \
         to accept that.",
    ))
}

fn configured_accounts(site: &SiteState) -> Result<AccountSet, ApiError> {
    accounts::read(site.site()?.store())
        .map_err(account_error)?
        .map(|(set, _)| set)
        .ok_or_else(|| {
            ApiError::conflict(
                "no_accounts",
                "This site has no accounts, so there is nothing to sign in to. \
                 Create the first one with `ridal site account add <name> \
                 --server-admin`.",
            )
        })
}

fn issue_session(
    site: &SiteState,
    account: &Account,
) -> Result<[(header::HeaderName, String); 1], ApiError> {
    let key = site
        .session_key()
        .map_err(|e| ApiError::internal("session_key_failed", e))?;
    let ttl = SESSION_TTL_DAYS * 24 * 60 * 60;
    let value = key.mint(&account.name, account.credential_version, auth::now() + ttl);
    Ok([(header::SET_COOKIE, auth::session_cookie(&value, ttl))])
}

/// `POST /api/v1/auth/login`
///
/// One refusal for every failure, and a decoy verification for a name that
/// does not exist, so the form cannot be used to enumerate accounts or to
/// time which ones exist.
async fn login(
    State(site): State<Arc<SiteState>>,
    Json(body): Json<LoginBody>,
) -> Result<impl IntoResponse, ApiError> {
    password_login_allowed(&site)?;
    let set = configured_accounts(&site)?;
    let refused = || {
        ApiError::unauthorized(
            "invalid_credentials",
            "That name and password do not match an account. If you were sent an \
             invite link, open that instead -- it is what sets your password the \
             first time.",
        )
    };

    let account = UserId::new(&body.name).ok().and_then(|name| set.get(&name));
    let verified = match account {
        Some(account) if account.is_activated() => {
            accounts::verify_password(account, &body.password)
        }
        _ => {
            burn_a_verification(&body.password);
            false
        }
    };
    if !verified {
        return Err(refused());
    }
    let account = account.expect("verified implies an account");

    Ok((
        issue_session(&site, account)?,
        Json(serde_json::json!({
            "user": account.name.as_str(),
            "server_admin": account.server_admin,
        })),
    ))
}

/// Do the work a real verification would, and discard the answer, so a miss
/// costs what a hit costs.
fn burn_a_verification(candidate: &str) {
    static DECOY: std::sync::OnceLock<Option<Account>> = std::sync::OnceLock::new();
    let decoy = DECOY.get_or_init(|| {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).ok()?;
        let hash = accounts::hash_password(&accounts::to_hex(&bytes)).ok()?;
        let mut account = Account::new(UserId::new("decoy").ok()?, false);
        account.password_hash = Some(hash);
        Some(account)
    });
    if let Some(decoy) = decoy {
        let _ = accounts::verify_password(decoy, candidate);
    }
}

/// `POST /api/v1/auth/logout`
async fn logout() -> impl IntoResponse {
    (
        StatusCode::OK,
        [(header::SET_COOKIE, auth::cleared_cookie())],
        Json(serde_json::json!({ "signed_out": true })),
    )
}

/// `GET /api/v1/auth/me` -- who the site thinks is calling.
async fn me(State(site): State<Arc<SiteState>>, caller: SiteCaller) -> impl IntoResponse {
    let configured = site.accounts_configured();
    Json(serde_json::json!({
        "user": caller.user.as_ref().map(|u| u.as_str()),
        "authenticated": caller.user.is_some(),
        "server_admin": caller.server_admin,
        "authentication_configured": configured,
    }))
}

fn account_for_invite<'a>(set: &'a AccountSet, token: &str, now: i64) -> Option<&'a Account> {
    set.users.iter().find(|account| {
        account
            .invite
            .as_ref()
            .is_some_and(|invite| invite.is_valid_at(now) && invite::token_matches(invite, token))
    })
}

#[derive(serde::Deserialize)]
pub struct RedeemBody {
    token: String,
    password: String,
}

/// `POST /api/v1/auth/invite` -- set a password with a one-time token.
///
/// Redeeming consumes the token, sets the password and bumps the credential
/// version, then adds the membership the invite carried, if it named a
/// project. Signs the person in on success.
async fn redeem_invite(
    State(site): State<Arc<SiteState>>,
    Json(body): Json<RedeemBody>,
) -> Result<impl IntoResponse, ApiError> {
    password_login_allowed(&site)?;
    accounts::check_password(&body.password).map_err(account_error)?;
    let stale = || {
        ApiError::bad_request(
            "invalid_invite",
            "This link is not valid. It may already have been used, or it may \
             have expired -- ask an administrator for a new one.",
        )
    };
    // The same refusal as an `AccountError`, for the copy that runs inside
    // `accounts::update`, which cannot return an `ApiError`.
    let stale_in_store = || {
        AccountError::Rejected(
            "This link is not valid. It may already have been used, or it may \
             have expired -- ask an administrator for a new one."
                .to_string(),
        )
    };

    // Checked before the expensive hash, and again under the store's lock
    // below, so an unauthenticated caller cannot spend a hash per request on
    // a token that was never valid.
    {
        let set = configured_accounts(&site)?;
        if account_for_invite(&set, &body.token, auth::now()).is_none() {
            return Err(stale());
        }
    }
    let hash = accounts::hash_password(&body.password).map_err(account_error)?;

    let (account, target) = accounts::update(site.site()?.store(), |set| {
        let name = account_for_invite(set, &body.token, auth::now())
            .map(|account| account.name.clone())
            .ok_or_else(stale_in_store)?;
        let account = set
            .get_mut(&name)
            .ok_or_else(|| AccountError::NotFound(name.to_string()))?;
        let target = account.invite.clone();
        account.password_hash = Some(hash.clone());
        account.invite = None;
        account.credential_version += 1;
        Ok((account.clone(), target))
    })
    .map_err(account_error)?;

    if let Some(membership) = &target {
        if let Some(key) = &membership.project {
            let role = membership.role.unwrap_or(Role::Picker);
            let download = membership.download.unwrap_or(DownloadScope::All);
            // The password is set by now, so a project deleted since the
            // invite was sent costs the membership, not the account.
            match site.site()?.project(key) {
                Ok(project) => {
                    members::update(project.documents(), |set| {
                        set.upsert(&account.name, role, download);
                        Ok(())
                    })
                    .map_err(|e| ApiError::internal("membership_write_failed", e.to_string()))?;
                }
                Err(SiteError::NotFound(_)) => {}
                Err(e) => return Err(site_error(e)),
            }
        }
    }
    // The person is the actor: they set their own password.
    let redeemer = account.name.as_str().to_string();
    audit(
        &site,
        site_audit::Entry::new(
            &redeemer,
            site_audit::Action::AccountActivated,
            account.name.as_str(),
        ),
    );
    if let Some(membership) = &target {
        if let Some(key) = &membership.project {
            audit(
                &site,
                site_audit::Entry::new(
                    &redeemer,
                    site_audit::Action::MembershipAdded,
                    account.name.as_str(),
                )
                .project(key)
                .membership(
                    membership.role.unwrap_or(Role::Picker),
                    membership.download.unwrap_or(DownloadScope::All),
                ),
            );
        }
    }

    Ok((
        issue_session(&site, &account)?,
        Json(serde_json::json!({
            "user": account.name.as_str(),
            "server_admin": account.server_admin,
        })),
    ))
}

// ---------------------------------------------------------------------------
// Projects API
// ---------------------------------------------------------------------------

async fn site_info(
    State(site): State<Arc<SiteState>>,
    caller: SiteCaller,
) -> Result<impl IntoResponse, ApiError> {
    let configured = site.accounts_configured();
    Ok(Json(serde_json::json!({
        "name": site.name(),
        "accounts_configured": configured,
        "authenticated": caller.user.is_some(),
        "user": caller.user.as_ref().map(|u| u.as_str()),
        "server_admin": caller.server_admin,
        "roles": Role::ALL.map(Role::as_str),
        "download_scopes": DownloadScope::ALL.map(DownloadScope::as_str),
        "invite_ttl_days": invite::INVITE_TTL_DAYS,
        "min_password_len": accounts::MIN_PASSWORD_LEN,
    })))
}

/// How much history one request returns. The log keeps roughly the last
/// two [`crate::project::jsonl::MAX_BYTES`]; a page shows the recent end of it.
const AUDIT_PAGE: usize = 500;

/// `GET /api/v1/site/audit` -- the site's account and project history, for a
/// server administrator.
async fn site_audit_log(
    State(site): State<Arc<SiteState>>,
    caller: SiteCaller,
) -> Result<impl IntoResponse, ApiError> {
    site.require_server_admin(&caller, "read the history")?;
    let log = site_audit::read(site.site()?.store())
        .map_err(|e| ApiError::internal("audit_read_failed", e.to_string()))?;
    let entries: Vec<&site_audit::Entry> = log.entries.iter().rev().take(AUDIT_PAGE).collect();
    Ok(Json(serde_json::json!({ "entries": entries })))
}

/// `GET /api/v1/projects/{key}/audit` -- the history of one project, for the
/// people who administer it. Only the entries that name this project.
async fn project_audit_log(
    State(site): State<Arc<SiteState>>,
    caller: SiteCaller,
    Path(key): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let key = parse_key(&key)?;
    require_project_admin(&site, &key, &caller, "read the history")?;
    let log = site_audit::read(site.site()?.store())
        .map_err(|e| ApiError::internal("audit_read_failed", e.to_string()))?;
    let entries: Vec<&site_audit::Entry> = log
        .entries
        .iter()
        .rev()
        .filter(|entry| entry.project.as_ref() == Some(&key))
        .take(AUDIT_PAGE)
        .collect();
    Ok(Json(serde_json::json!({ "entries": entries })))
}

#[derive(serde::Deserialize)]
pub struct SitePreferencesBody {
    /// `light`, `dark`, or absent/`null`/empty to follow the device.
    #[serde(default)]
    theme: Option<String>,
}

/// A caller with an account but nowhere to keep settings: `ridal gui` over a
/// bare directory of radargrams, which is not a project.
fn no_preferences() -> ApiError {
    ApiError::conflict(
        "not_a_project",
        "This catalog is not a Ridal project, so there is nowhere to keep settings.",
    )
}

/// `GET /api/v1/site/preferences` -- the caller's own site-wide settings.
async fn get_site_preferences(
    State(site): State<Arc<SiteState>>,
    caller: SiteCaller,
) -> Result<impl IntoResponse, ApiError> {
    let Some(user) = caller.user.as_ref() else {
        return Err(ApiError::unauthorized(
            "authentication_required",
            "Sign in to read your site settings.",
        ));
    };
    let store = site.preferences_store().ok_or_else(no_preferences)?;
    let preferences = crate::project::preferences::read(store, user)
        .map_err(|e| ApiError::internal("preferences_read_failed", e.to_string()))?;
    Ok(Json(serde_json::json!({
        "user": user.as_str(),
        "theme": preferences.theme,
    })))
}

/// `PUT /api/v1/site/preferences`
///
/// A viewer may do this, exactly as with project preferences: choosing how
/// you like to look at something is the floor of having an account.
async fn put_site_preferences(
    State(site): State<Arc<SiteState>>,
    caller: SiteCaller,
    Json(body): Json<SitePreferencesBody>,
) -> Result<impl IntoResponse, ApiError> {
    let Some(user) = caller.user.as_ref() else {
        return Err(ApiError::unauthorized(
            "authentication_required",
            "Sign in to change your site settings.",
        ));
    };
    let store = site.preferences_store().ok_or_else(no_preferences)?;
    let mut stored = crate::project::preferences::read_lenient(store, user);
    stored.theme = match body.theme.as_deref() {
        None | Some("") => None,
        Some(name) => {
            if !super::routes::is_offered_theme(name) {
                return Err(ApiError::bad_request(
                    "unknown_theme",
                    format!(
                        "'{name}' is not a theme. Use 'light', 'dark', or nothing at all \
                         to follow the device."
                    ),
                ));
            }
            Some(name.to_string())
        }
    };
    crate::project::preferences::write(store, user, &stored, &Expectation::Any)
        .map_err(|e| ApiError::internal("preferences_write_failed", e.to_string()))?;
    Ok(Json(serde_json::json!({
        "user": user.as_str(),
        "theme": stored.theme,
    })))
}

/// `GET /api/v1/projects`
async fn list_projects(
    State(site): State<Arc<SiteState>>,
    caller: SiteCaller,
) -> Result<impl IntoResponse, ApiError> {
    let keys = site.site()?.list().map_err(site_error)?;
    let projects: Vec<serde_json::Value> = keys
        .iter()
        .filter_map(|key| project_entry(&site, key, &caller, Lookup::Listing))
        .collect();
    Ok(Json(serde_json::json!({ "projects": projects })))
}

#[derive(serde::Deserialize)]
pub struct CreateProjectBody {
    key: String,
    #[serde(default)]
    name: Option<String>,
}

/// `POST /api/v1/projects`
async fn create_project(
    State(site): State<Arc<SiteState>>,
    caller: SiteCaller,
    Json(body): Json<CreateProjectBody>,
) -> Result<impl IntoResponse, ApiError> {
    site.require_server_admin(&caller, "create a project")?;
    site.require_writable("create a project")?;
    let key = parse_key(&body.key)?;
    let project = site
        .site()?
        .create_project(&key, body.name.as_deref(), caller.user.as_ref())
        .map_err(site_error)?;
    audit(
        &site,
        site_audit::Entry::new(
            actor(&caller),
            site_audit::Action::ProjectCreated,
            key.as_str(),
        ),
    );
    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({
            "key": key.as_str(),
            "name": project.config().project.name,
        })),
    ))
}

/// `GET /api/v1/projects/{key}`
async fn project_info(
    State(site): State<Arc<SiteState>>,
    caller: SiteCaller,
    Path(key): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let key = parse_key(&key)?;
    project_entry(&site, &key, &caller, Lookup::ByKey)
        .map(Json)
        .ok_or_else(hidden_project)
}

#[derive(serde::Deserialize)]
pub struct UpdateProjectBody {
    #[serde(default)]
    name: Option<String>,
}

/// `PATCH /api/v1/projects/{key}` -- rename the display name.
async fn update_project(
    State(site): State<Arc<SiteState>>,
    caller: SiteCaller,
    Path(key): Path<String>,
    Json(body): Json<UpdateProjectBody>,
) -> Result<impl IntoResponse, ApiError> {
    site.require_server_admin(&caller, "rename a project")?;
    site.require_writable("rename a project")?;
    let key = parse_key(&key)?;
    site.site()?
        .project(&key)
        .map_err(site_error)?
        .set_name(body.name.as_deref())
        .map_err(|e| ApiError::internal("project_write_failed", e.to_string()))?;
    site.evict(&key);
    audit(
        &site,
        site_audit::Entry::new(
            actor(&caller),
            site_audit::Action::ProjectRenamed,
            key.as_str(),
        ),
    );
    Ok(Json(serde_json::json!({ "key": key.as_str() })))
}

/// `POST /api/v1/projects/{key}/archive`
async fn archive_project(
    State(site): State<Arc<SiteState>>,
    caller: SiteCaller,
    Path(key): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    site.require_server_admin(&caller, "archive a project")?;
    site.require_writable("archive a project")?;
    let key = parse_key(&key)?;
    site.site()?.archive(&key).map_err(site_error)?;
    site.evict(&key);
    audit(
        &site,
        site_audit::Entry::new(
            actor(&caller),
            site_audit::Action::ProjectArchived,
            key.as_str(),
        ),
    );
    Ok(Json(
        serde_json::json!({ "key": key.as_str(), "archived": true }),
    ))
}

/// `POST /api/v1/projects/{key}/unarchive`
async fn unarchive_project(
    State(site): State<Arc<SiteState>>,
    caller: SiteCaller,
    Path(key): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    site.require_server_admin(&caller, "unarchive a project")?;
    site.require_writable("unarchive a project")?;
    let key = parse_key(&key)?;
    site.site()?.unarchive(&key).map_err(site_error)?;
    site.evict(&key);
    audit(
        &site,
        site_audit::Entry::new(
            actor(&caller),
            site_audit::Action::ProjectUnarchived,
            key.as_str(),
        ),
    );
    Ok(Json(serde_json::json!({
        "key": key.as_str(),
        "archived": false,
    })))
}

/// `DELETE /api/v1/projects/{key}`
async fn delete_project(
    State(site): State<Arc<SiteState>>,
    caller: SiteCaller,
    Path(key): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    site.require_server_admin(&caller, "delete a project")?;
    site.require_writable("delete a project")?;
    let key = parse_key(&key)?;
    site.evict(&key);
    site.site()?.delete_project(&key).map_err(site_error)?;
    audit(
        &site,
        site_audit::Entry::new(
            actor(&caller),
            site_audit::Action::ProjectDeleted,
            key.as_str(),
        ),
    );
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Membership and access API
// ---------------------------------------------------------------------------

/// How a caller stands towards one project.
enum Standing {
    /// A server administrator: an administrator in every project.
    ServerAdmin,
    Member(members::Member),
    /// Not a member, but the project may be read without a login.
    Public,
    /// Not a member of a project that requires a login. Answered exactly as
    /// a project that does not exist, so its existence does not leak.
    Hidden,
}

/// Open `key` and work out the caller's [`Standing`] in it.
///
/// A membership file that will not parse reads as closed (see
/// [`members::read_for_access`]), the same as the project's own middleware.
fn standing(
    site: &SiteState,
    key: &ProjectKey,
    caller: &SiteCaller,
) -> Result<(crate::project::Project, members::MemberSet, Standing), ApiError> {
    let project = site
        .site()
        .map_err(|_| hidden_project())?
        .project(key)
        .map_err(site_error)?;
    let members = members::read_for_access(project.documents());
    let standing = if caller.server_admin {
        Standing::ServerAdmin
    } else if let Some(member) = caller.user.as_ref().and_then(|name| members.get(name)) {
        Standing::Member(member.clone())
    } else if members.require_auth_to_read {
        Standing::Hidden
    } else {
        Standing::Public
    };
    Ok((project, members, standing))
}

/// Refuse unless the caller administers `key` -- as a server administrator,
/// or as a project member with the `admin` role -- and return the project.
///
/// A caller who may not see the project gets the same 404 as for one that
/// does not exist; one who may see it is told what they lack.
fn require_project_admin(
    site: &SiteState,
    key: &ProjectKey,
    caller: &SiteCaller,
    action: &str,
) -> Result<crate::project::Project, ApiError> {
    let (project, _, standing) = standing(site, key, caller)?;
    match standing {
        Standing::ServerAdmin => Ok(project),
        Standing::Member(member) if member.role == Role::Admin => Ok(project),
        Standing::Hidden => Err(hidden_project()),
        _ if caller.user.is_none() => Err(ApiError::unauthorized(
            "authentication_required",
            format!("Sign in as a project administrator to {action}."),
        )),
        _ => Err(ApiError::forbidden(
            "insufficient_role",
            format!("Only a project administrator may {action}."),
        )),
    }
}

fn member_json(member: &members::Member) -> serde_json::Value {
    serde_json::json!({
        "name": member.name.as_str(),
        "role": member.role.as_str(),
        "download": member.download.as_str(),
    })
}

/// `GET /api/v1/projects/{key}/members`
async fn list_members(
    State(site): State<Arc<SiteState>>,
    caller: SiteCaller,
    Path(key): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let key = parse_key(&key)?;
    let project = require_project_admin(&site, &key, &caller, "see the members")?;
    let configured = members::is_configured(project.documents()).map_err(member_error)?;
    let set = members::read(project.documents())
        .map_err(member_error)?
        .map(|(set, _)| set)
        .unwrap_or_default();
    Ok(Json(serde_json::json!({
        "members": set.members.iter().map(member_json).collect::<Vec<_>>(),
        "require_auth_to_read": set.require_auth_to_read,
        "anonymous_download": set.anonymous_download.as_str(),
        "configured": configured,
        "has_admin": set.has_admin(),
        "roles": Role::ALL.map(Role::as_str),
        "download_scopes": DownloadScope::ALL.map(DownloadScope::as_str),
    })))
}

#[derive(serde::Deserialize)]
pub struct MemberBody {
    name: String,
    role: String,
    #[serde(default)]
    download: Option<String>,
}

fn parse_member_body(body: &MemberBody) -> Result<(UserId, Role, DownloadScope), ApiError> {
    let name = parse_name(&body.name, "invalid_member")?;
    let role = Role::parse(&body.role).map_err(|e| ApiError::bad_request("invalid_role", e))?;
    let download = body
        .download
        .as_deref()
        .map(DownloadScope::parse)
        .transpose()
        .map_err(|e| ApiError::bad_request("invalid_download_scope", e))?
        .unwrap_or_default();
    Ok((name, role, download))
}

/// `POST /api/v1/projects/{key}/members` -- add or update a membership.
async fn add_member(
    State(site): State<Arc<SiteState>>,
    caller: SiteCaller,
    Path(key): Path<String>,
    Json(body): Json<MemberBody>,
) -> Result<impl IntoResponse, ApiError> {
    let key = parse_key(&key)?;
    let project = require_project_admin(&site, &key, &caller, "add a member")?;
    site.require_project_writable(&key, "add a member")?;
    let (name, role, download) = parse_member_body(&body)?;

    // A membership names a site account, so the account must exist. Without
    // this a typo would write a member nobody can sign in as.
    let accounts = account_set(&site)?;
    if accounts.get(&name).is_none() {
        return Err(ApiError::not_found(
            "account_not_found",
            format!(
                "No site account named '{name}'. Press 'Invite new member' to \
                 create the account, or check the spelling."
            ),
        ));
    }

    let added = members::update(project.documents(), |set| {
        Ok(set.upsert(&name, role, download))
    })
    .map_err(member_error)?;
    audit(
        &site,
        site_audit::Entry::new(
            actor(&caller),
            if added {
                site_audit::Action::MembershipAdded
            } else {
                site_audit::Action::MembershipChanged
            },
            name.as_str(),
        )
        .project(&key)
        .membership(role, download),
    );

    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({
            "name": name.as_str(),
            "role": role.as_str(),
            "download": download.as_str(),
        })),
    ))
}

/// `POST /api/v1/projects/{key}/members/invite` -- create a site account and
/// invite it into this project.
///
/// A project administrator may create an account, but only for their own
/// project: the invite carries this project, role and download and nothing
/// else. The account is never a server administrator, and account lifecycle
/// -- deleting it, resetting its password, promoting it -- stays with a
/// server administrator at the site level.
async fn invite_member(
    State(site): State<Arc<SiteState>>,
    caller: SiteCaller,
    Path(key): Path<String>,
    Json(body): Json<MemberBody>,
) -> Result<impl IntoResponse, ApiError> {
    let key = parse_key(&key)?;
    let project = require_project_admin(&site, &key, &caller, "invite a member")?;
    site.require_project_writable(&key, "invite a member")?;
    let (name, role, download) = parse_member_body(&body)?;

    let set = account_set(&site)?;
    if set.get(&name).is_some() {
        return Err(ApiError::conflict(
            "account_exists",
            format!(
                "'{name}' already has an account. Press 'Add member' to give them a role here."
            ),
        ));
    }
    reusable_name(&site, &name)?;

    let (token, invite) = invite::mint_for_project(auth::now(), key.clone(), role, download)
        .map_err(|e| ApiError::internal("invite_failed", e))?;

    // Created without server administration, whatever else this route can
    // carry. The invite would otherwise hand the whole site to whoever
    // redeemed the link.
    accounts::update(site.site()?.store(), |set| {
        if set.get(&name).is_some() {
            return Err(AccountError::Duplicate(name.to_string()));
        }
        let mut account = Account::new(name.clone(), false);
        account.invite = Some(invite.clone());
        set.users.push(account);
        Ok(())
    })
    .map_err(account_error)?;

    members::update(project.documents(), |set| {
        set.upsert(&name, role, download);
        Ok(())
    })
    .map_err(member_error)?;
    audit(
        &site,
        site_audit::Entry::new(
            actor(&caller),
            site_audit::Action::AccountCreated,
            name.as_str(),
        )
        .project(&key)
        .note("created by a project administrator"),
    );
    audit(
        &site,
        site_audit::Entry::new(
            actor(&caller),
            site_audit::Action::MembershipAdded,
            name.as_str(),
        )
        .project(&key)
        .membership(role, download),
    );

    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({
            "name": name.as_str(),
            "role": role.as_str(),
            "download": download.as_str(),
            "invite_path": format!("/invite/{token}"),
            "invite_expires": invite.expires,
            "invite_ttl_days": invite::INVITE_TTL_DAYS,
        })),
    ))
}

#[derive(serde::Deserialize)]
pub struct UpdateMemberBody {
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    download: Option<String>,
}

/// `PUT /api/v1/projects/{key}/members/{name}`
async fn update_member(
    State(site): State<Arc<SiteState>>,
    caller: SiteCaller,
    Path((key, name)): Path<(String, String)>,
    Json(body): Json<UpdateMemberBody>,
) -> Result<impl IntoResponse, ApiError> {
    let key = parse_key(&key)?;
    let project = require_project_admin(&site, &key, &caller, "change a member")?;
    site.require_project_writable(&key, "change a member")?;
    let name = parse_name(&name, "invalid_member")?;
    let role = body
        .role
        .as_deref()
        .map(Role::parse)
        .transpose()
        .map_err(|e| ApiError::bad_request("invalid_role", e))?;
    let download = body
        .download
        .as_deref()
        .map(DownloadScope::parse)
        .transpose()
        .map_err(|e| ApiError::bad_request("invalid_download_scope", e))?;

    let updated = members::update(project.documents(), |set| {
        if role.is_some_and(|role| role < Role::Admin) && set.is_last_admin(&name) {
            return Err(members::MemberError::Rejected(format!(
                "'{name}' is the only administrator of this project. Promote \
                 someone else first, or a server administrator can still \
                 manage it."
            )));
        }
        let member = set
            .get_mut(&name)
            .ok_or_else(|| members::MemberError::NotFound(name.to_string()))?;
        if let Some(role) = role {
            member.role = role;
        }
        if let Some(download) = download {
            member.download = download;
        }
        Ok(member.clone())
    })
    .map_err(member_error)?;
    audit(
        &site,
        site_audit::Entry::new(
            actor(&caller),
            site_audit::Action::MembershipChanged,
            name.as_str(),
        )
        .project(&key)
        .membership(updated.role, updated.download),
    );

    Ok(Json(member_json(&updated)))
}

/// `DELETE /api/v1/projects/{key}/members/{name}`
async fn remove_member(
    State(site): State<Arc<SiteState>>,
    caller: SiteCaller,
    Path((key, name)): Path<(String, String)>,
) -> Result<impl IntoResponse, ApiError> {
    let key = parse_key(&key)?;
    let project = require_project_admin(&site, &key, &caller, "remove a member")?;
    site.require_project_writable(&key, "remove a member")?;
    let name = parse_name(&name, "invalid_member")?;

    members::update(project.documents(), |set| {
        if set.is_last_admin(&name) {
            return Err(members::MemberError::Rejected(format!(
                "'{name}' is the only administrator of this project. Promote \
                 someone else first."
            )));
        }
        set.members.retain(|member| member.name != name);
        Ok(())
    })
    .map_err(member_error)?;
    audit(
        &site,
        site_audit::Entry::new(
            actor(&caller),
            site_audit::Action::MembershipRemoved,
            name.as_str(),
        )
        .project(&key),
    );
    Ok(StatusCode::NO_CONTENT)
}

#[derive(serde::Deserialize)]
pub struct AccessBody {
    #[serde(default)]
    require_auth_to_read: Option<bool>,
    #[serde(default)]
    anonymous_download: Option<String>,
}

/// `PUT /api/v1/projects/{key}/access` -- the project-wide access policy.
async fn put_project_access(
    State(site): State<Arc<SiteState>>,
    caller: SiteCaller,
    Path(key): Path<String>,
    Json(body): Json<AccessBody>,
) -> Result<impl IntoResponse, ApiError> {
    let key = parse_key(&key)?;
    let project = require_project_admin(&site, &key, &caller, "change the access settings")?;
    site.require_project_writable(&key, "change the access settings")?;
    let anonymous_download = body
        .anonymous_download
        .as_deref()
        .map(DownloadScope::parse)
        .transpose()
        .map_err(|e| ApiError::bad_request("invalid_download_scope", e))?;

    let set = members::update(project.documents(), |set| {
        if let Some(require) = body.require_auth_to_read {
            set.require_auth_to_read = require;
        }
        if let Some(scope) = anonymous_download {
            set.anonymous_download = scope;
        }
        Ok(set.clone())
    })
    .map_err(member_error)?;
    audit(
        &site,
        site_audit::Entry::new(
            actor(&caller),
            site_audit::Action::AccessChanged,
            key.as_str(),
        )
        .note(format!(
            "require_auth_to_read={}, anonymous_download={}",
            set.require_auth_to_read,
            set.anonymous_download.as_str()
        )),
    );

    Ok(Json(serde_json::json!({
        "require_auth_to_read": set.require_auth_to_read,
        "anonymous_download": set.anonymous_download.as_str(),
    })))
}

// ---------------------------------------------------------------------------
// Accounts API
// ---------------------------------------------------------------------------

/// `GET /api/v1/accounts`
async fn list_accounts(
    State(site): State<Arc<SiteState>>,
    caller: SiteCaller,
) -> Result<impl IntoResponse, ApiError> {
    site.require_server_admin(&caller, "see the accounts")?;
    let set = account_set(&site)?;
    Ok(Json(serde_json::json!({
        "accounts": set.users.iter().map(Account::redacted).collect::<Vec<_>>(),
        "invite_ttl_days": invite::INVITE_TTL_DAYS,
        "min_password_len": accounts::MIN_PASSWORD_LEN,
        "account_file": ACCOUNTS_FILE,
    })))
}

/// `GET /api/v1/site/memberships` -- every account and the projects it
/// belongs to, for a read-only overview on the site settings page.
async fn site_memberships(
    State(site): State<Arc<SiteState>>,
    caller: SiteCaller,
) -> Result<impl IntoResponse, ApiError> {
    site.require_server_admin(&caller, "see memberships")?;
    let accounts = account_set(&site)?;
    let keys = site.site()?.list().map_err(site_error)?;

    // Seeded from the accounts so one with no memberships still appears.
    let mut by_account: std::collections::HashMap<String, Vec<serde_json::Value>> = accounts
        .users
        .iter()
        .map(|account| (account.name.as_str().to_string(), Vec::new()))
        .collect();
    for key in &keys {
        // A project that no longer opens is skipped rather than failing the
        // overview; this is a convenience page, not an authority.
        let Ok(project) = site.site()?.project(key) else {
            continue;
        };
        let name = project
            .config()
            .project
            .name
            .clone()
            .unwrap_or_else(|| key.as_str().to_string());
        let set = members::read(project.documents())
            .map_err(member_error)?
            .map(|(set, _)| set)
            .unwrap_or_default();
        for member in &set.members {
            // A membership may name an account that no longer exists.
            if let Some(list) = by_account.get_mut(member.name.as_str()) {
                list.push(serde_json::json!({
                    "project": key.as_str(),
                    "project_name": name,
                    "role": member.role.as_str(),
                    "download": member.download.as_str(),
                }));
            }
        }
    }

    let overview: Vec<serde_json::Value> = accounts
        .users
        .iter()
        .map(|account| {
            let memberships = by_account.remove(account.name.as_str()).unwrap_or_default();
            serde_json::json!({
                "name": account.name.as_str(),
                "memberships": memberships,
            })
        })
        .collect();
    Ok(Json(serde_json::json!({ "accounts": overview })))
}

#[derive(serde::Deserialize)]
pub struct CreateAccountBody {
    name: String,
    #[serde(default)]
    server_admin: bool,
    /// When set, the invite grants a membership in this project on
    /// redemption. A server administrator may instead invite with no
    /// project.
    #[serde(default)]
    project: Option<String>,
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    download: Option<String>,
}

/// `POST /api/v1/accounts` -- create an account and mint its invite.
async fn create_account(
    State(site): State<Arc<SiteState>>,
    caller: SiteCaller,
    Json(body): Json<CreateAccountBody>,
) -> Result<impl IntoResponse, ApiError> {
    site.require_server_admin(&caller, "create an account")?;
    site.require_writable("create an account")?;
    // A new account gets an invite, and anyone holding that link can claim
    // the account. Creating it as a server administrator would therefore
    // hand the whole site to whoever used the link, so the flag can only be
    // set later, on an account that has a password.
    if body.server_admin {
        return Err(ApiError::bad_request(
            "server_admin_at_creation",
            "Create the account first, then grant server administration from \
             the accounts list once it has a password. A pending invite that \
             carried administrator rights would hand the site to whoever used \
             the link.",
        ));
    }
    let name = parse_name(&body.name, "invalid_account")?;
    let role = body
        .role
        .as_deref()
        .map(Role::parse)
        .transpose()
        .map_err(|e| ApiError::bad_request("invalid_role", e))?
        .unwrap_or_default();
    let download = body
        .download
        .as_deref()
        .map(DownloadScope::parse)
        .transpose()
        .map_err(|e| ApiError::bad_request("invalid_download_scope", e))?
        .unwrap_or_default();
    let project = resolve_optional_project(&site, body.project.as_deref())?;

    let (token, invite) = match &project {
        Some(key) => invite::mint_for_project(auth::now(), key.clone(), role, download),
        None => invite::mint(auth::now(), None, None, None),
    }
    .map_err(|e| ApiError::internal("invite_failed", e))?;
    let expires = invite.expires;

    let created = accounts::update(site.site()?.store(), |set| {
        if set.get(&name).is_some() {
            return Err(AccountError::Duplicate(name.to_string()));
        }
        let mut account = Account::new(name.clone(), false);
        account.invite = Some(invite.clone());
        set.users.push(account.clone());
        Ok(account)
    })
    .map_err(account_error)?;

    let mut entry = site_audit::Entry::new(
        actor(&caller),
        site_audit::Action::AccountCreated,
        name.as_str(),
    );
    if let Some(key) = &project {
        entry = entry.project(key).membership(role, download);
    }
    audit(&site, entry);

    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({
            "account": created.redacted(),
            "invite_path": format!("/invite/{token}"),
            "invite_expires": expires,
            "invite_ttl_days": invite::INVITE_TTL_DAYS,
        })),
    ))
}

#[derive(serde::Deserialize)]
pub struct UpdateAccountBody {
    #[serde(default)]
    server_admin: Option<bool>,
}

/// `PUT /api/v1/accounts/{name}` -- change the server-admin flag.
///
/// Bumps the credential version, so a demotion reaches a live session on its
/// next request rather than when the cookie ages out.
async fn update_account(
    State(site): State<Arc<SiteState>>,
    caller: SiteCaller,
    Path(name): Path<String>,
    Json(body): Json<UpdateAccountBody>,
) -> Result<impl IntoResponse, ApiError> {
    site.require_server_admin(&caller, "change an account")?;
    site.require_writable("change an account")?;
    let name = parse_name(&name, "invalid_account")?;

    let (redacted, changed) = accounts::update(site.site()?.store(), |set| {
        if let Some(admin) = body.server_admin {
            let is_admin = set.get(&name).map(|a| a.server_admin).unwrap_or(false);
            if is_admin && !admin && !set.has_another_admin(&name) {
                return Err(AccountError::Rejected(format!(
                    "'{name}' is the only server administrator. Promote someone \
                     else first, or nobody will be able to manage accounts."
                )));
            }
            // Promoting an account that still has an invite outstanding
            // would hand administration to whoever redeems the link.
            if admin {
                if let Some(account) = set.get(&name) {
                    if !account.is_activated() || account.invite.is_some() {
                        return Err(AccountError::Rejected(format!(
                            "'{name}' has not set a password yet, or has a reset \
                             outstanding. Wait until the current invite has been \
                             used before granting administrator rights."
                        )));
                    }
                }
            }
        }
        let account = set
            .get_mut(&name)
            .ok_or_else(|| AccountError::NotFound(name.to_string()))?;
        let mut changed = false;
        if let Some(admin) = body.server_admin {
            if account.server_admin != admin {
                account.server_admin = admin;
                account.credential_version += 1;
                changed = true;
            }
        }
        Ok((account.redacted(), changed))
    })
    .map_err(account_error)?;
    if changed {
        audit(
            &site,
            site_audit::Entry::new(
                actor(&caller),
                if body.server_admin == Some(true) {
                    site_audit::Action::ServerAdminGranted
                } else {
                    site_audit::Action::ServerAdminRevoked
                },
                name.as_str(),
            ),
        );
    }

    Ok(Json(redacted))
}

/// `DELETE /api/v1/accounts/{name}`
///
/// Removes the account and its membership in every project, never any
/// project's picks. A later account of the same name starts with nothing.
async fn delete_account(
    State(site): State<Arc<SiteState>>,
    caller: SiteCaller,
    Path(name): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    site.require_server_admin(&caller, "remove an account")?;
    site.require_writable("remove an account")?;
    let name = parse_name(&name, "invalid_account")?;
    let removed_from = site.site()?.remove_account(&name).map_err(site_error)?;
    for key in &removed_from {
        audit(
            &site,
            site_audit::Entry::new(
                actor(&caller),
                site_audit::Action::MembershipRemoved,
                name.as_str(),
            )
            .project(key)
            .note("account removed"),
        );
    }
    audit(
        &site,
        site_audit::Entry::new(
            actor(&caller),
            site_audit::Action::AccountRemoved,
            name.as_str(),
        ),
    );
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/v1/accounts/{name}/invite` -- reissue, for a reset or a lost
/// link.
async fn reissue_account_invite(
    State(site): State<Arc<SiteState>>,
    caller: SiteCaller,
    Path(name): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    site.require_server_admin(&caller, "reset a password")?;
    site.require_writable("reset a password")?;
    let name = parse_name(&name, "invalid_account")?;
    let (token, invite) = invite::mint(auth::now(), None, None, None)
        .map_err(|e| ApiError::internal("invite_failed", e))?;
    let expires = invite.expires;

    accounts::update(site.site()?.store(), |set| {
        let account = set
            .get_mut(&name)
            .ok_or_else(|| AccountError::NotFound(name.to_string()))?;
        account.invite = Some(invite.clone());
        Ok(())
    })
    .map_err(account_error)?;
    audit(
        &site,
        site_audit::Entry::new(
            actor(&caller),
            site_audit::Action::InviteIssued,
            name.as_str(),
        ),
    );

    Ok(Json(serde_json::json!({
        "user": name.as_str(),
        "invite_path": format!("/invite/{token}"),
        "invite_expires": expires,
        "invite_ttl_days": invite::INVITE_TTL_DAYS,
    })))
}

/// A bulk creation request, shared in shape by the invite and password
/// routes so the two read the same way.
#[derive(serde::Deserialize)]
pub struct BulkAccountsBody {
    /// Ignored when `random_names` is set, and optional so a request can ask
    /// for random accounts without sending an unused prefix.
    #[serde(default)]
    prefix: String,
    count: usize,
    #[serde(default)]
    random_names: bool,
    role: String,
    #[serde(default)]
    download: Option<String>,
    /// When set, each invite grants this membership on redemption; a batch
    /// of generated passwords is added to the project directly, since there
    /// is no redemption step.
    #[serde(default)]
    project: Option<String>,
}

#[derive(serde::Deserialize)]
pub struct BulkPasswordsBody {
    #[serde(default)]
    prefix: String,
    count: usize,
    #[serde(default)]
    random_names: bool,
    role: String,
    #[serde(default)]
    download: Option<String>,
    #[serde(default)]
    project: Option<String>,
    #[serde(default)]
    acknowledge_risk: bool,
}

fn parse_bulk_role_download(
    role: &str,
    download: Option<&str>,
) -> Result<(Role, DownloadScope), ApiError> {
    let role = Role::parse(role).map_err(|e| ApiError::bad_request("invalid_role", e))?;
    let download = download
        .map(DownloadScope::parse)
        .transpose()
        .map_err(|e| ApiError::bad_request("invalid_download_scope", e))?
        .unwrap_or_default();
    Ok((role, download))
}

/// Refuse to hand a project administrator a name that some project already
/// has a membership for.
///
/// Whoever gets an account inherits every membership its name has, so a
/// name left behind in a project copied into the site would otherwise let
/// the administrator of one project invite themselves into another. A
/// server administrator may still reuse one deliberately, which is how a
/// copied-in project's members are reconnected.
fn reusable_name(site: &SiteState, name: &UserId) -> Result<(), ApiError> {
    if site
        .site()?
        .member_names()
        .map_err(site_error)?
        .contains(name)
    {
        return Err(ApiError::conflict(
            "name_in_use",
            format!(
                "'{name}' is already used on this site. Choose another name, or ask \
                 a server administrator."
            ),
        ));
    }
    Ok(())
}

/// A batch of one-time invite links, shared by the site route (any project,
/// or none) and the project route (this project only).
async fn bulk_invites(
    site: Arc<SiteState>,
    project: Option<ProjectKey>,
    actor: String,
    body: BulkAccountsBody,
) -> Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    let (role, download) = parse_bulk_role_download(&body.role, body.download.as_deref())?;
    let directory = site.site()?;
    let prefix = (!body.random_names).then_some(body.prefix.as_str());
    let names = directory
        .batch_names(prefix, body.count)
        .map_err(site_error)?;
    let grant = Grant {
        project: project.clone(),
        role,
        download,
    };
    let minted = directory.invite_batch(&names, &grant).map_err(site_error)?;

    for (name, _, _) in &minted {
        let mut entry =
            site_audit::Entry::new(&actor, site_audit::Action::AccountCreated, name.as_str())
                .note("bulk invite");
        if let Some(key) = &project {
            entry = entry.project(key);
        }
        audit(&site, entry);
    }

    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({
            "users": minted.iter().map(|(name, token, expires)| serde_json::json!({
                "name": name.as_str(),
                "invite_path": format!("/invite/{token}"),
                "invite_expires": expires,
            })).collect::<Vec<_>>(),
            "invite_ttl_days": invite::INVITE_TTL_DAYS,
        })),
    ))
}

/// `POST /api/v1/accounts/bulk/invites` -- a batch of one-time invite links.
async fn create_bulk_invites(
    State(site): State<Arc<SiteState>>,
    caller: SiteCaller,
    Json(body): Json<BulkAccountsBody>,
) -> Result<impl IntoResponse, ApiError> {
    site.require_server_admin(&caller, "create accounts")?;
    site.require_writable("create accounts")?;
    let project = resolve_optional_project(&site, body.project.as_deref())?;
    bulk_invites(site, project, actor(&caller), body).await
}

/// `POST /api/v1/projects/{key}/members/bulk/invites` -- a project
/// administrator's batch, scoped to their project.
async fn create_project_bulk_invites(
    State(site): State<Arc<SiteState>>,
    caller: SiteCaller,
    Path(key): Path<String>,
    Json(body): Json<BulkAccountsBody>,
) -> Result<impl IntoResponse, ApiError> {
    let key = parse_key(&key)?;
    require_project_admin(&site, &key, &caller, "create accounts")?;
    site.require_project_writable(&key, "create accounts")?;
    bulk_invites(site, Some(key), actor(&caller), body).await
}

/// A batch of accounts with generated passwords, shared by the site route
/// and the project route.
async fn bulk_passwords(
    site: Arc<SiteState>,
    project: Option<ProjectKey>,
    actor: String,
    body: BulkPasswordsBody,
) -> Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    if !body.acknowledge_risk {
        return Err(ApiError::bad_request(
            "risk_acknowledgement_required",
            "Bulk passwords are less safe than invite links. Set acknowledge_risk \
             to true only if you understand the risk.",
        ));
    }
    let (role, download) = parse_bulk_role_download(&body.role, body.download.as_deref())?;
    let advisory = bulk::bulk_risk_advisory(role).ok_or_else(|| {
        ApiError::bad_request(
            "admin_bulk_passwords_forbidden",
            "Administrator accounts must be created with one-time invite links, \
             not shared passwords.",
        )
    })?;
    let prefix = (!body.random_names).then_some(body.prefix.as_str());
    let names = site
        .site()?
        .batch_names(prefix, body.count)
        .map_err(site_error)?;
    let grant = Grant {
        project: project.clone(),
        role,
        download,
    };
    // One Argon2id hash per account: off the async runtime.
    let hashing = Arc::clone(&site);
    let generated = tokio::task::spawn_blocking(move || {
        hashing
            .site()?
            .password_batch(&names, &grant)
            .map_err(site_error)
    })
    .await
    .map_err(|e| ApiError::internal("password_hash_task_failed", e.to_string()))??;

    for (name, _) in &generated {
        let mut entry =
            site_audit::Entry::new(&actor, site_audit::Action::AccountCreated, name.as_str())
                .note("bulk generated password");
        if let Some(key) = &project {
            entry = entry.project(key).membership(role, download);
        }
        audit(&site, entry);
    }

    Ok((
        StatusCode::OK,
        Json(serde_json::json!({
            "users": generated.iter().map(|(name, password)| serde_json::json!({
                "name": name.as_str(),
                "password": password,
            })).collect::<Vec<_>>(),
            "advisory": advisory,
        })),
    ))
}

/// `POST /api/v1/accounts/bulk/passwords` -- accounts with generated
/// passwords, for a workshop where handing out links is impractical.
async fn create_bulk_passwords(
    State(site): State<Arc<SiteState>>,
    caller: SiteCaller,
    Json(body): Json<BulkPasswordsBody>,
) -> Result<impl IntoResponse, ApiError> {
    site.require_server_admin(&caller, "create accounts")?;
    site.require_writable("create accounts")?;
    let project = resolve_optional_project(&site, body.project.as_deref())?;
    bulk_passwords(site, project, actor(&caller), body).await
}

/// `POST /api/v1/projects/{key}/members/bulk/passwords` -- a project
/// administrator's batch, scoped to their project.
async fn create_project_bulk_passwords(
    State(site): State<Arc<SiteState>>,
    caller: SiteCaller,
    Path(key): Path<String>,
    Json(body): Json<BulkPasswordsBody>,
) -> Result<impl IntoResponse, ApiError> {
    let key = parse_key(&key)?;
    require_project_admin(&site, &key, &caller, "create accounts")?;
    site.require_project_writable(&key, "create accounts")?;
    bulk_passwords(site, Some(key), actor(&caller), body).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_project_path_is_rewritten_to_its_router_shape() {
        let (key, path) = split_project_path("/api/v1/projects/glac/datasets").unwrap();
        assert_eq!(key.as_str(), "glac");
        assert_eq!(path, "/api/v1/datasets");

        let (_, path) = split_project_path("/api/v1/projects/glac").unwrap();
        assert_eq!(path, "/api/v1");

        let (_, path) = split_project_path("/api/v1/projects/glac/settings").unwrap();
        assert_eq!(path, "/api/v1/settings");

        let (key, path) = split_project_path("/p/glac/view/line-01").unwrap();
        assert_eq!(key.as_str(), "glac");
        assert_eq!(path, "/view/line-01");

        let (_, path) = split_project_path("/p/glac").unwrap();
        assert_eq!(path, "/");
    }

    #[test]
    fn a_path_that_names_no_project_is_not_rewritten() {
        assert!(split_project_path("/api/v1/datasets").is_none());
        assert!(split_project_path("/login").is_none());
        // A malformed key is not a project.
        assert!(split_project_path("/p/Not A Key/x").is_none());
    }
}
