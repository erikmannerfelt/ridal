//! The multi-project site server (#214).
//!
//! One entry point serves many projects from one directory. [`SiteState`]
//! holds the site's registry, its accounts and one shared render budget.
//! Each project is served by the *unchanged*
//! [`build_router`](super::app::build_router): a request to the site's
//! `/p/{key}/…` or `/api/v1/projects/{key}/…` is rewritten back to the
//! project-relative path and dispatched in-process with
//! [`tower::ServiceExt::oneshot`]. Keeping every project handler and route
//! literal as it was is what keeps `app::http_reference_tests` and the
//! single-project GUI valid.
//!
//! # Identity
//!
//! Site accounts live in `accounts.json` at the site root; a project holds
//! only memberships (`members.json`, still filed as `users.json`). The
//! project router's own middleware resolves a [`Caller`](super::auth::Caller)
//! from both, because each project's [`AppState`] carries a
//! [`SiteContext`] -- so member roles and the 404-for-non-members rule reach
//! every project route without a handler changing. This module's
//! [`SiteCaller`] is only for the site-level routes: the landing page, the
//! account routes, and signing in.

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
use super::auth::{self, SessionKey, SESSION_COOKIE, SESSION_TTL_DAYS};
use super::render_service::RenderServiceConfig;
use super::routes::{ApiError, PageError};
use super::templates;
use crate::identity::{ProjectKey, UserId};
use crate::project::members;
use crate::project::store::{DocumentStore, Expectation};
use crate::project::users::{self, DownloadScope, Role};
use crate::site::accounts::{self, invite, Account, AccountError, AccountSet};
use crate::site::{audit as site_audit, Site, SiteError};

/// The site's account file, for the "create the first one" hint.
const ACCOUNTS_FILE: &str = accounts::ACCOUNTS_FILE;

/// A project's built state and router, cached until it is evicted.
///
/// Built lazily on the first request that names the key, so starting a site
/// with a hundred projects opens none of them until someone looks.
pub struct ProjectRuntime {
    /// Kept so a later request can rediscover this project in place without
    /// rebuilding it from disk (#147's `rediscover`, per project).
    #[allow(
        dead_code,
        reason = "per-project rediscovery lands with the site settings routes"
    )]
    pub state: Arc<AppState>,
    pub router: Router,
}

/// Everything the site server needs that is not per-project.
pub struct SiteState {
    pub site: Site,
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
            site,
            access,
            // `.max(1)`: a zero-permit semaphore would deadlock every render.
            render_permits: Arc::new(Semaphore::new(render_config.n_workers.max(1))),
            render_config,
            projects: RwLock::new(HashMap::new()),
            session_key: Mutex::new(None),
        })
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
        let key = SessionKey::load_or_create(self.site.store())?;
        *guard = Some(key.clone());
        Ok(key)
    }

    /// The runtime for `key`, building and caching it on first use.
    pub fn runtime(&self, key: &ProjectKey) -> Result<Arc<ProjectRuntime>, ApiError> {
        if let Ok(cache) = self.projects.read() {
            if let Some(runtime) = cache.get(key) {
                return Ok(Arc::clone(runtime));
            }
        }

        let project = self.site.project(key).map_err(site_error)?;
        let archived = self.site.is_archived(key);
        let root = project.root().to_path_buf();
        let state =
            AppState::build_with_project(&root, &self.render_config, Some(project), self.access)
                .map_err(|e| ApiError::internal("project_open_failed", e))?
                .with_site(Arc::new(SiteContext::new(
                    key.clone(),
                    DocumentStore::new(self.site.root().to_path_buf()),
                    archived,
                )))
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

    /// Drop a cached project, so the next request rebuilds it. Used after an
    /// archive change, a rename or a delete.
    fn evict(&self, key: &ProjectKey) {
        if let Ok(mut cache) = self.projects.write() {
            cache.remove(key);
        }
    }
}

fn site_error(error: SiteError) -> ApiError {
    match &error {
        SiteError::NotFound(_) => ApiError::not_found("project_not_found", error.to_string()),
        SiteError::KeyInUse(_) => ApiError::conflict("project_exists", error.to_string()),
        _ => ApiError::internal("site_error", error.to_string()),
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
    site_audit::record(site.site.store(), entry);
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
    let key = ProjectKey::new(raw).map_err(|e| ApiError::bad_request("invalid_project_key", e))?;
    if !site.site.project_path(&key).is_dir() {
        return Err(ApiError::not_found(
            "project_not_found",
            format!("No project '{key}' in this site."),
        ));
    }
    Ok(Some(key))
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
}

impl SiteCaller {
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
/// anonymous caller remains.
fn resolve_caller(site: &SiteState, headers: &HeaderMap, now: i64) -> SiteCaller {
    let accounts = match accounts::read(site.site.store()) {
        Ok(Some((set, _))) => Some(set),
        Ok(None) => None,
        Err(_) => Some(AccountSet::default()),
    };
    let account = accounts.as_ref().and_then(|set| {
        let cookie = auth::cookie_value(headers, SESSION_COOKIE)?;
        let key = site.session_key().ok()?;
        let (name, version) = key.verify(&cookie, now)?;
        let account = set.get(&name)?;
        (account.credential_version == version).then_some(account)
    });
    SiteCaller {
        user: account.map(|account| account.name.clone()),
        server_admin: account.is_some_and(|account| account.server_admin),
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

/// Rewrite a site path to the project-relative path its router expects.
///
/// `/p/{key}/view/x` becomes `/view/x`; `/api/v1/projects/{key}/datasets`
/// becomes `/api/v1/datasets`. The one irregular mapping is the project
/// settings document: the site spells it `/api/v1/projects/{key}/settings`
/// and the project router spells it `/api/v1/project/settings`, so the
/// singular segment is put back here rather than renamed in the router.
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
        if tail.is_empty() {
            "/api/v1".to_string()
        } else if tail == "/settings" {
            "/api/v1/project/settings".to_string()
        } else {
            format!("/api/v1{tail}")
        }
    } else if tail.is_empty() {
        "/".to_string()
    } else {
        tail
    };
    Some((key, rewritten))
}

/// Routes that exist on a project router but must not be reachable through a
/// site, because they would read or write the membership file as though it
/// still held accounts. Site identity is server-wide and lives under
/// `/api/v1/auth/*` and `/api/v1/accounts*` at the site root instead.
fn is_retired_project_path(path: &str) -> bool {
    path.starts_with("/api/v1/users")
        || path == "/api/v1/access"
        || path == "/api/v1/auth"
        || path.starts_with("/api/v1/auth/")
        || path == "/login"
        || path == "/invite"
        || path.starts_with("/invite/")
}

async fn project_fallback(State(site): State<Arc<SiteState>>, request: Request) -> Response {
    let path = request.uri().path().to_string();
    let Some((key, rewritten)) = split_project_path(&path) else {
        return ApiError::not_found("not_found", "No such page.").into_response();
    };
    if is_retired_project_path(&rewritten) {
        return ApiError::not_found(
            "not_found",
            "That endpoint belongs to the old per-project accounts, which a site \
             replaces with site accounts.",
        )
        .into_response();
    }
    let runtime = match site.runtime(&key) {
        Ok(runtime) => runtime,
        Err(error) => return error.into_response(),
    };

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
    let Some(user) = user else {
        return String::new();
    };
    crate::project::preferences::read_lenient(site.site.store(), user)
        .theme
        .filter(|theme| super::routes::is_offered_theme(theme))
        .unwrap_or_default()
}

/// `GET /` -- the landing page.
///
/// A site that has accounts asks for a login first; one that does not (a
/// read-only public catalog) shows its projects to anyone.
async fn landing(
    State(site): State<Arc<SiteState>>,
    caller: SiteCaller,
) -> Result<Response, PageError> {
    let configured = accounts::is_configured(site.site.store()).unwrap_or(false);
    if configured && caller.user.is_none() {
        return Ok(Redirect::to("/login").into_response());
    }

    let keys = site
        .site
        .list()
        .map_err(|e| PageError(ApiError::internal("site_error", e.to_string())))?;
    let projects: Vec<serde_json::Value> = keys
        .iter()
        .filter_map(|key| project_entry(&site, key, &caller))
        .collect();

    let (api_base, site_api_base, page_base) = site_page_bases();
    let html = render(
        "site.html.jinja",
        minijinja::context! {
            site_name => site.site.name(),
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

/// One project as the landing list and the API describe it, or `None` when
/// the caller may not see it.
fn project_entry(
    site: &SiteState,
    key: &ProjectKey,
    caller: &SiteCaller,
) -> Option<serde_json::Value> {
    let project = site.site.project(key).ok()?;
    let members = members::read(project.documents())
        .ok()
        .flatten()
        .map(|(set, _)| set)
        .unwrap_or_default();
    let member = caller.user.as_ref().and_then(|name| members.get(name));
    let visible = caller.server_admin || member.is_some() || !members.require_auth_to_read;
    if !visible {
        return None;
    }
    let (role, download) = match member {
        Some(member) => (member.role, member.download),
        None if caller.server_admin => (Role::Admin, DownloadScope::All),
        None => (Role::Viewer, members.anonymous_download),
    };
    Some(serde_json::json!({
        "key": key.as_str(),
        "name": project
            .config()
            .project
            .name
            .unwrap_or_else(|| key.as_str().to_string()),
        "archived": site.site.is_archived(key),
        "member": member.is_some() || caller.server_admin,
        "member_count": members.members.len(),
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
async fn site_settings_page(
    State(site): State<Arc<SiteState>>,
    caller: SiteCaller,
) -> Result<Response, PageError> {
    let configured = accounts::is_configured(site.site.store()).unwrap_or(false);
    if caller.user.is_none() {
        return Ok(Redirect::to(if configured { "/login" } else { "/" }).into_response());
    }

    let keys = site
        .site
        .list()
        .map_err(|e| PageError(ApiError::internal("site_error", e.to_string())))?;
    let projects: Vec<serde_json::Value> = keys
        .iter()
        .filter_map(|key| project_entry(&site, key, &caller))
        .collect();

    let (api_base, site_api_base, page_base) = site_page_bases();
    let html = render(
        "site_settings.html.jinja",
        minijinja::context! {
            site_name => site.site.name(),
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
    let configured = accounts::is_configured(site.site.store()).unwrap_or(false);
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
    accounts::read(site.site.store())
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
        let hash = accounts::hash_password(&users::to_hex(&bytes)).ok()?;
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
    let configured = accounts::is_configured(site.site.store()).unwrap_or(false);
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

    let (account, target) = accounts::update(site.site.store(), |set| {
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
            let project = site.site.project(key).map_err(site_error)?;
            members::update(project.documents(), |set| {
                match set.get_mut(&account.name) {
                    Some(member) => {
                        member.role = role;
                        member.download = download;
                    }
                    None => {
                        set.members
                            .push(members::Member::new(account.name.clone(), role, download))
                    }
                }
                Ok(())
            })
            .map_err(|e| ApiError::internal("membership_write_failed", e.to_string()))?;
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
    let configured = accounts::is_configured(site.site.store()).map_err(account_error)?;
    Ok(Json(serde_json::json!({
        "name": site.site.name(),
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

/// How much history one request returns. The log keeps the most recent
/// [`site_audit::MAX_ENTRIES`]; a page shows the recent end of it.
const AUDIT_PAGE: usize = 500;

/// `GET /api/v1/site/audit` -- the site's account and project history, for a
/// server administrator.
async fn site_audit_log(
    State(site): State<Arc<SiteState>>,
    caller: SiteCaller,
) -> Result<impl IntoResponse, ApiError> {
    caller.require_server_admin("read the history")?;
    let (log, _) = site_audit::read(site.site.store())
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
    let key = ProjectKey::new(&key).map_err(|e| ApiError::bad_request("invalid_project_key", e))?;
    require_project_admin(&site, &key, &caller, "read the history")?;
    let (log, _) = site_audit::read(site.site.store())
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
    let preferences = crate::project::preferences::read(site.site.store(), user)
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
    let mut stored = crate::project::preferences::read_lenient(site.site.store(), user);
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
    crate::project::preferences::write(site.site.store(), user, &stored, &Expectation::Any)
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
    let keys = site.site.list().map_err(site_error)?;
    let projects: Vec<serde_json::Value> = keys
        .iter()
        .filter_map(|key| project_entry(&site, key, &caller))
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
    caller.require_server_admin("create a project")?;
    let key =
        ProjectKey::new(&body.key).map_err(|e| ApiError::bad_request("invalid_project_key", e))?;
    let project = site
        .site
        .create_project(&key, body.name.as_deref())
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
    let key = ProjectKey::new(&key).map_err(|e| ApiError::bad_request("invalid_project_key", e))?;
    project_entry(&site, &key, &caller)
        .map(Json)
        .ok_or_else(|| {
            ApiError::not_found(
                "project_not_found",
                "No such project, or you are not a member of it.",
            )
        })
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
    caller.require_server_admin("rename a project")?;
    let key = ProjectKey::new(&key).map_err(|e| ApiError::bad_request("invalid_project_key", e))?;
    let project = site.site.project(&key).map_err(site_error)?;
    project
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
    caller.require_server_admin("archive a project")?;
    let key = ProjectKey::new(&key).map_err(|e| ApiError::bad_request("invalid_project_key", e))?;
    site.site.archive(&key).map_err(site_error)?;
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
    caller.require_server_admin("unarchive a project")?;
    let key = ProjectKey::new(&key).map_err(|e| ApiError::bad_request("invalid_project_key", e))?;
    site.site.unarchive(&key).map_err(site_error)?;
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
    caller.require_server_admin("delete a project")?;
    let key = ProjectKey::new(&key).map_err(|e| ApiError::bad_request("invalid_project_key", e))?;
    site.evict(&key);
    site.site.delete_project(&key).map_err(site_error)?;
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

/// Refuse unless the caller administers `key` -- as a server administrator,
/// or as a project member with the `admin` role.
fn require_project_admin(
    site: &SiteState,
    key: &ProjectKey,
    caller: &SiteCaller,
    action: &str,
) -> Result<(), ApiError> {
    if caller.server_admin {
        return Ok(());
    }
    let Some(user) = caller.user.as_ref() else {
        return Err(ApiError::unauthorized(
            "authentication_required",
            format!("Sign in as a project administrator to {action}."),
        ));
    };
    let project = site.site.project(key).map_err(site_error)?;
    let members = members::read(project.documents())
        .map_err(member_error)?
        .map(|(set, _)| set)
        .unwrap_or_default();
    if members
        .get(user)
        .is_some_and(|member| member.role == Role::Admin)
    {
        return Ok(());
    }
    Err(ApiError::forbidden(
        "insufficient_role",
        format!("Only a project administrator may {action}."),
    ))
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
    let key = ProjectKey::new(&key).map_err(|e| ApiError::bad_request("invalid_project_key", e))?;
    require_project_admin(&site, &key, &caller, "see the members")?;
    let project = site.site.project(&key).map_err(site_error)?;
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
    let name = UserId::new(&body.name).map_err(|e| ApiError::bad_request("invalid_member", e))?;
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
    let key = ProjectKey::new(&key).map_err(|e| ApiError::bad_request("invalid_project_key", e))?;
    require_project_admin(&site, &key, &caller, "add a member")?;
    let (name, role, download) = parse_member_body(&body)?;

    // A membership names a site account, so the account must exist. Without
    // this a typo would write a member nobody can sign in as.
    let accounts = accounts::read(site.site.store())
        .map_err(account_error)?
        .map(|(set, _)| set)
        .unwrap_or_default();
    if accounts.get(&name).is_none() {
        return Err(ApiError::not_found(
            "account_not_found",
            format!("No site account named '{name}'. Create it first."),
        ));
    }

    let project = site.site.project(&key).map_err(site_error)?;
    let added = std::cell::Cell::new(false);
    members::update(project.documents(), |set| {
        match set.get_mut(&name) {
            Some(member) => {
                member.role = role;
                member.download = download;
            }
            None => {
                added.set(true);
                set.members
                    .push(members::Member::new(name.clone(), role, download));
            }
        }
        Ok(())
    })
    .map_err(member_error)?;
    audit(
        &site,
        site_audit::Entry::new(
            actor(&caller),
            if added.get() {
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
    let key = ProjectKey::new(&key).map_err(|e| ApiError::bad_request("invalid_project_key", e))?;
    require_project_admin(&site, &key, &caller, "invite a member")?;
    let (name, role, download) = parse_member_body(&body)?;

    let set = accounts::read(site.site.store())
        .map_err(account_error)?
        .map(|(set, _)| set)
        .unwrap_or_default();
    if set.get(&name).is_some() {
        return Err(ApiError::conflict(
            "account_exists",
            format!("'{name}' already has an account. Add them as a member instead."),
        ));
    }

    let (token, invite) = invite::mint_for_project(auth::now(), key.clone(), role, download)
        .map_err(|e| ApiError::internal("invite_failed", e))?;

    // Created without server administration, whatever else this route can
    // carry. The invite would otherwise hand the whole site to whoever
    // redeemed the link.
    accounts::update(site.site.store(), |set| {
        if set.get(&name).is_some() {
            return Err(AccountError::Duplicate(name.to_string()));
        }
        let mut account = Account::new(name.clone(), false);
        account.invite = Some(invite.clone());
        set.users.push(account);
        Ok(())
    })
    .map_err(account_error)?;

    let project = site.site.project(&key).map_err(site_error)?;
    members::update(project.documents(), |set| {
        match set.get_mut(&name) {
            Some(member) => {
                member.role = role;
                member.download = download;
            }
            None => set
                .members
                .push(members::Member::new(name.clone(), role, download)),
        }
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
    let key = ProjectKey::new(&key).map_err(|e| ApiError::bad_request("invalid_project_key", e))?;
    require_project_admin(&site, &key, &caller, "change a member")?;
    let name = UserId::new(&name).map_err(|e| ApiError::bad_request("invalid_member", e))?;
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

    let project = site.site.project(&key).map_err(site_error)?;
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
    let key = ProjectKey::new(&key).map_err(|e| ApiError::bad_request("invalid_project_key", e))?;
    require_project_admin(&site, &key, &caller, "remove a member")?;
    let name = UserId::new(&name).map_err(|e| ApiError::bad_request("invalid_member", e))?;

    let project = site.site.project(&key).map_err(site_error)?;
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
    let key = ProjectKey::new(&key).map_err(|e| ApiError::bad_request("invalid_project_key", e))?;
    require_project_admin(&site, &key, &caller, "change the access settings")?;
    let anonymous_download = body
        .anonymous_download
        .as_deref()
        .map(DownloadScope::parse)
        .transpose()
        .map_err(|e| ApiError::bad_request("invalid_download_scope", e))?;

    let project = site.site.project(&key).map_err(site_error)?;
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
    caller.require_server_admin("see the accounts")?;
    let set = accounts::read(site.site.store())
        .map_err(account_error)?
        .map(|(set, _)| set)
        .unwrap_or_default();
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
    caller.require_server_admin("see memberships")?;
    let accounts = accounts::read(site.site.store())
        .map_err(account_error)?
        .map(|(set, _)| set)
        .unwrap_or_default();
    let keys = site.site.list().map_err(site_error)?;

    // Seeded from the accounts so one with no memberships still appears.
    let mut by_account: std::collections::HashMap<String, Vec<serde_json::Value>> = accounts
        .users
        .iter()
        .map(|account| (account.name.as_str().to_string(), Vec::new()))
        .collect();
    for key in &keys {
        // A project that no longer opens is skipped rather than failing the
        // overview; this is a convenience page, not an authority.
        let Ok(project) = site.site.project(key) else {
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
    caller.require_server_admin("create an account")?;
    let name = UserId::new(&body.name).map_err(|e| ApiError::bad_request("invalid_account", e))?;
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

    let created = accounts::update(site.site.store(), |set| {
        if set.get(&name).is_some() {
            return Err(AccountError::Duplicate(name.to_string()));
        }
        let mut account = Account::new(name.clone(), body.server_admin);
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
    if body.server_admin {
        entry = entry.note("server administrator");
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
    caller.require_server_admin("change an account")?;
    let name = UserId::new(&name).map_err(|e| ApiError::bad_request("invalid_account", e))?;

    let (redacted, changed) = accounts::update(site.site.store(), |set| {
        if let Some(admin) = body.server_admin {
            let is_admin = set.get(&name).map(|a| a.server_admin).unwrap_or(false);
            if is_admin && !admin && !set.has_another_admin(&name) {
                return Err(AccountError::Rejected(format!(
                    "'{name}' is the only server administrator. Promote someone \
                     else first, or nobody will be able to manage accounts."
                )));
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
/// Removes the account, never any project's picks. Memberships in projects
/// are left in place: they name an account that no longer exists, which
/// resolves to nothing, and recreating the same name reconnects them.
async fn delete_account(
    State(site): State<Arc<SiteState>>,
    caller: SiteCaller,
    Path(name): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    caller.require_server_admin("remove an account")?;
    let name = UserId::new(&name).map_err(|e| ApiError::bad_request("invalid_account", e))?;
    accounts::update(site.site.store(), |set| {
        let Some(account) = set.get(&name) else {
            return Err(AccountError::NotFound(name.to_string()));
        };
        if account.server_admin && !set.has_another_admin(&name) {
            return Err(AccountError::Rejected(format!(
                "'{name}' is the only server administrator. Promote someone else \
                 first, or nobody will be able to manage accounts."
            )));
        }
        set.users.retain(|account| account.name != name);
        Ok(())
    })
    .map_err(account_error)?;
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
    caller.require_server_admin("reset a password")?;
    let name = UserId::new(&name).map_err(|e| ApiError::bad_request("invalid_account", e))?;
    let (token, invite) = invite::mint(auth::now(), None, None, None)
        .map_err(|e| ApiError::internal("invite_failed", e))?;
    let expires = invite.expires;

    accounts::update(site.site.store(), |set| {
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

fn bulk_error(error: users::UserError) -> ApiError {
    ApiError::bad_request("invalid_bulk_accounts", error.to_string())
}

/// The names a batch will create, drawn from the site's existing accounts.
fn bulk_account_names(
    set: &AccountSet,
    prefix: &str,
    count: usize,
    random_names: bool,
) -> Result<Vec<UserId>, ApiError> {
    if random_names {
        users::random_bulk_names_from(set.users.iter().map(|account| &account.name), count)
            .map_err(bulk_error)
    } else {
        let start =
            users::next_bulk_start_from(set.users.iter().map(|account| &account.name), prefix);
        users::bulk_names_after(prefix, count, start).map_err(bulk_error)
    }
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
    let set = accounts::read(site.site.store())
        .map_err(account_error)?
        .map(|(set, _)| set)
        .unwrap_or_default();
    let names = bulk_account_names(&set, &body.prefix, body.count, body.random_names)?;

    let minted: Vec<(UserId, String, invite::Invite)> = names
        .iter()
        .map(|name| {
            let (token, invite) = match &project {
                Some(key) => invite::mint_for_project(auth::now(), key.clone(), role, download),
                None => invite::mint(auth::now(), None, None, None),
            }
            .map_err(|e| ApiError::internal("invite_failed", e))?;
            Ok((name.clone(), token, invite))
        })
        .collect::<Result<_, ApiError>>()?;

    accounts::update(site.site.store(), |set| {
        if let Some((name, _, _)) = minted.iter().find(|(name, _, _)| set.get(name).is_some()) {
            return Err(AccountError::Duplicate(name.to_string()));
        }
        for (name, _, invite) in &minted {
            let mut account = Account::new(name.clone(), false);
            account.invite = Some(invite.clone());
            set.users.push(account);
        }
        Ok(())
    })
    .map_err(account_error)?;

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
            "users": minted.iter().map(|(name, token, invite)| serde_json::json!({
                "name": name.as_str(),
                "invite_path": format!("/invite/{token}"),
                "invite_expires": invite.expires,
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
    caller.require_server_admin("create accounts")?;
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
    let key = ProjectKey::new(&key).map_err(|e| ApiError::bad_request("invalid_project_key", e))?;
    require_project_admin(&site, &key, &caller, "create accounts")?;
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
    let advisory = users::bulk_risk_advisory(role).ok_or_else(|| {
        ApiError::bad_request(
            "admin_bulk_passwords_forbidden",
            "Administrator accounts must be created with one-time invite links, \
             not shared passwords.",
        )
    })?;
    let set = accounts::read(site.site.store())
        .map_err(account_error)?
        .map(|(set, _)| set)
        .unwrap_or_default();
    let names = bulk_account_names(&set, &body.prefix, body.count, body.random_names)?;

    let generated: Vec<(UserId, String)> = names
        .iter()
        .map(|name| users::generate_password().map(|password| (name.clone(), password)))
        .collect::<Result<_, _>>()
        .map_err(bulk_error)?;
    let to_hash = generated.clone();
    let hashed = tokio::task::spawn_blocking(move || {
        to_hash
            .into_iter()
            .map(|(name, password)| accounts::hash_password(&password).map(|hash| (name, hash)))
            .collect::<Result<Vec<_>, AccountError>>()
    })
    .await
    .map_err(|e| ApiError::internal("password_hash_task_failed", e.to_string()))?
    .map_err(account_error)?;

    accounts::update(site.site.store(), |set| {
        if let Some((name, _)) = hashed.iter().find(|(name, _)| set.get(name).is_some()) {
            return Err(AccountError::Duplicate(name.to_string()));
        }
        for ((name, _password), (_, hash)) in generated.iter().zip(&hashed) {
            let mut account = Account::new(name.clone(), false);
            account.password_hash = Some(hash.clone());
            set.users.push(account);
        }
        Ok(())
    })
    .map_err(account_error)?;

    // There is no invite to redeem, so a named project's membership is added
    // now rather than on redemption.
    if let Some(key) = &project {
        let project = site.site.project(key).map_err(site_error)?;
        members::update(project.documents(), |set| {
            for name in &names {
                match set.get_mut(name) {
                    Some(member) => {
                        member.role = role;
                        member.download = download;
                    }
                    None => set
                        .members
                        .push(members::Member::new(name.clone(), role, download)),
                }
            }
            Ok(())
        })
        .map_err(member_error)?;
    }

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
    caller.require_server_admin("create accounts")?;
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
    let key = ProjectKey::new(&key).map_err(|e| ApiError::bad_request("invalid_project_key", e))?;
    require_project_admin(&site, &key, &caller, "create accounts")?;
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

        // The one irregular mapping: the settings document.
        let (_, path) = split_project_path("/api/v1/projects/glac/settings").unwrap();
        assert_eq!(path, "/api/v1/project/settings");

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

    #[test]
    fn the_old_project_account_routes_are_retired() {
        for path in [
            "/api/v1/users",
            "/api/v1/users/bulk/invites",
            "/api/v1/users/anna",
            "/api/v1/access",
            "/api/v1/auth/login",
            "/api/v1/auth/invite",
            "/login",
            "/invite/abc",
        ] {
            assert!(is_retired_project_path(path), "{path}");
        }
        for path in ["/api/v1/datasets", "/api/v1/preferences", "/settings", "/"] {
            assert!(!is_retired_project_path(path), "{path}");
        }
    }
}
