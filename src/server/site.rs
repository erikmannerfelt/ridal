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
use axum::routing::get;
use axum::{Json, Router};
use tokio::sync::Semaphore;
use tower::ServiceExt;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use super::app::{AccessOptions, AppState, SiteContext};
use super::auth::{self, SessionKey, SESSION_TTL_DAYS};
use super::needs::{Needs, Who};
use super::render_service::RenderServiceConfig;
use super::routes::{ApiError, ErrorBody, PageError};
use super::templates;
use crate::identity::{ProjectKey, UserId};
use crate::project::members;
use crate::project::roles::{DownloadScope, Role};
use crate::project::store::{DocumentStore, Expectation};
use crate::site::accounts::{self, bulk, invite, Account, AccountError, AccountSet};
use crate::site::{audit as site_audit, tokens, Grant, Site, SiteError};

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
    /// Every processing job on this server, whichever project it is for:
    /// `--processing-jobs` and the other limits are site-wide (#382).
    pub processing: Arc<super::processing::ProcessingQueue>,
    /// The projects opened so far, keyed by their immutable key.
    projects: RwLock<HashMap<ProjectKey, Arc<ProjectRuntime>>>,
    /// The site-wide cookie-signing key, loaded on first use.
    session_key: Mutex<Option<SessionKey>>,
    /// Requests answered since the heartbeat last asked (#282).
    requests: std::sync::atomic::AtomicU64,
    /// When the heartbeat last started its interval counts, and when the
    /// server started (#282, #391).
    interval_started: Mutex<std::time::Instant>,
    started: std::time::Instant,
}

impl SiteState {
    pub fn new(
        site: Site,
        access: AccessOptions,
        render_config: RenderServiceConfig,
        processing: super::processing::ProcessingConfig,
    ) -> Arc<Self> {
        Arc::new(Self {
            processing: super::processing::ProcessingQueue::new(processing),
            host: Host::Site(site),
            access,
            // `.max(1)`: a zero-permit semaphore would deadlock every render.
            render_permits: Arc::new(Semaphore::new(render_config.n_workers.max(1))),
            render_config,
            projects: RwLock::new(HashMap::new()),
            session_key: Mutex::new(None),
            requests: Default::default(),
            interval_started: Mutex::new(std::time::Instant::now()),
            started: std::time::Instant::now(),
        })
    }

    /// `ridal gui`'s site of one: `state`, already built, as the project
    /// `default` (#214).
    pub fn local(state: AppState, processing: super::processing::ProcessingConfig) -> Arc<Self> {
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
        let processing = super::processing::ProcessingQueue::new(processing);
        let state = Arc::new(state.with_processing(Arc::clone(&processing)));
        let runtime = Arc::new(ProjectRuntime {
            router: super::app::build_router(Arc::clone(&state)),
            state,
        });
        let key = local_key();
        Arc::new(Self {
            processing,
            host: Host::Local { name, store },
            access,
            render_permits,
            render_config: runtime.state.render_config(),
            projects: RwLock::new(HashMap::from([(key, runtime)])),
            session_key: Mutex::new(None),
            requests: Default::default(),
            interval_started: Mutex::new(std::time::Instant::now()),
            started: std::time::Instant::now(),
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

    /// How every project's render services are configured: one cache,
    /// one set of permits, for the whole server.
    pub fn render_config(&self) -> &RenderServiceConfig {
        &self.render_config
    }

    /// Count one answered request, for the heartbeat.
    pub fn count_request(&self) {
        self.requests
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// Requests answered since [`Self::take_requests`] last started the
    /// count.
    pub fn requests(&self) -> u64 {
        self.requests.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Requests answered since the last call; the count starts again.
    pub fn take_requests(&self) -> u64 {
        self.requests.swap(0, std::sync::atomic::Ordering::Relaxed)
    }

    /// How long the current interval of counts has run, and, when `reset`,
    /// start a new one now.
    pub fn interval(&self, reset: bool) -> std::time::Duration {
        let mut started = self
            .interval_started
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = std::time::Instant::now();
        let elapsed = now.saturating_duration_since(*started);
        if reset {
            *started = now;
        }
        elapsed
    }

    /// How long this server has been running.
    pub fn uptime(&self) -> std::time::Duration {
        self.started.elapsed()
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
                .with_render_permits(Arc::clone(&self.render_permits))
                .with_processing(Arc::clone(&self.processing));
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
    /// Never true for a request with an API token, which does not carry
    /// server administration even when its account has it.
    pub server_admin: bool,
    /// Whether the site has an account file at all.
    pub accounts_configured: bool,
    /// The API token the request came with, if it did (#194).
    pub token: Option<SiteToken>,
}

/// An API token, as the site resolved it for one request (#194).
#[derive(Debug, Clone)]
pub struct SiteToken {
    pub id: String,
    pub name: String,
    pub grants: Vec<tokens::Grant>,
    pub expires: Option<i64>,
    /// Whether the token's account is a server administrator. In a granted
    /// project that makes it an administrator, as the account would be; it
    /// never makes the token one at the site level.
    pub account_server_admin: bool,
}

impl SiteToken {
    fn grant_for(&self, key: &ProjectKey) -> Option<&tokens::Grant> {
        self.grants.iter().find(|grant| &grant.project == key)
    }
}

impl SiteCaller {
    fn identity(&self) -> auth::SiteIdentity<'_> {
        auth::SiteIdentity {
            user: self.user.as_ref(),
            server_admin: self.server_admin,
            accounts_configured: self.accounts_configured,
            token: None,
        }
    }

    /// The caller as its account stands, for deciding which projects a
    /// token's account may see. Only for a token's granted projects: at the
    /// site level a token is never a server administrator.
    fn account_view(&self) -> SiteCaller {
        SiteCaller {
            server_admin: self
                .token
                .as_ref()
                .map_or(self.server_admin, |token| token.account_server_admin),
            token: None,
            ..self.clone()
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

/// The caller the middleware resolved, whether or not it came with a token.
fn resolved_caller(parts: &Parts) -> Result<SiteCaller, ApiError> {
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

/// Every site route refuses an API token unless it asks for
/// [`TokenOrSession`] instead (#194). Accounts, memberships, preferences and
/// tokens themselves are managed with a session, so a leaked token cannot
/// turn itself into lasting access, and a site route added later refuses
/// tokens without anyone having to remember to.
impl FromRequestParts<Arc<SiteState>> for SiteCaller {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        _state: &Arc<SiteState>,
    ) -> Result<Self, Self::Rejection> {
        let caller = resolved_caller(parts)?;
        if caller.token.is_some() {
            return Err(ApiError::forbidden(
                "token_not_allowed",
                "An API token cannot be used for this. It needs a signed-in \
                 session: accounts, memberships and tokens are managed from a \
                 browser or with `ridal site`.",
            ));
        }
        Ok(caller)
    }
}

/// A site caller that may have come with an API token, for the few site
/// routes a token may use: who am I, and which projects can I reach (#194).
pub struct TokenOrSession(pub SiteCaller);

impl FromRequestParts<Arc<SiteState>> for TokenOrSession {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        _state: &Arc<SiteState>,
    ) -> Result<Self, Self::Rejection> {
        resolved_caller(parts).map(TokenOrSession)
    }
}

/// Resolve a site caller from an API token, or else the session cookie, and
/// `accounts.json`.
///
/// A damaged account file fails closed: no account matches, so only an
/// anonymous caller remains. Under `ridal gui` there are no accounts, and
/// the one person is the local default user wherever there is a project to
/// keep their settings in; a token sent there is ignored, since there is
/// nothing it could add.
///
/// A token that does not hold is an error, never an anonymous caller: a
/// script whose token was revoked must be told, not quietly given whatever
/// the public may see (#194). A token wins over a cookie.
fn resolve_caller(site: &SiteState, headers: &HeaderMap, now: i64) -> Result<SiteCaller, ApiError> {
    let directory = match &site.host {
        Host::Site(directory) => directory,
        Host::Local { store, .. } => {
            return Ok(SiteCaller {
                user: store
                    .as_ref()
                    .and_then(|_| UserId::new(crate::identity::DEFAULT_USER).ok()),
                server_admin: false,
                accounts_configured: false,
                token: None,
            })
        }
    };
    let accounts = match accounts::read(directory.store()) {
        Ok(Some((set, _))) => Some(set),
        Ok(None) => None,
        Err(_) => Some(AccountSet::default()),
    };
    if let Some(presented) = bearer_token(headers) {
        return token_caller(site, directory, accounts.as_ref(), &presented, now);
    }
    let account = accounts.as_ref().and_then(|set| {
        let key = site.session_key().ok()?;
        auth::signed_in(set, &key, headers, now)
    });
    Ok(SiteCaller {
        user: account.map(|account| account.name.clone()),
        server_admin: account.is_some_and(|account| account.server_admin),
        accounts_configured: accounts.is_some(),
        token: None,
    })
}

/// The token in an `Authorization: Bearer` header. Only there: never a
/// cookie, which a browser would send by itself, nor a query string, which
/// ends up in logs.
fn bearer_token(headers: &HeaderMap) -> Option<String> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = value.trim().split_once(' ')?;
    scheme
        .eq_ignore_ascii_case("bearer")
        .then(|| token.trim().to_string())
}

/// The caller an API token stands for, or why it does not stand for one.
fn token_caller(
    site: &SiteState,
    directory: &Site,
    accounts: Option<&AccountSet>,
    presented: &str,
    now: i64,
) -> Result<SiteCaller, ApiError> {
    // A token is a password a script holds, so it travels under the same
    // rule as one.
    if !site.access.allow_password_login {
        return Err(insecure_transport("an API token"));
    }
    let invalid = || {
        ApiError::unauthorized(
            "invalid_token",
            "That API token is not valid: it is mistyped, revoked, or its account \
             is gone. Create a new one.",
        )
    };
    let (set, _) = tokens::read(directory.store()).map_err(|_| invalid())?;
    let token = tokens::authenticate(&set, presented, now).map_err(|refusal| match refusal {
        tokens::Refusal::Expired => ApiError::unauthorized(
            "token_expired",
            "That API token has expired. Create a new one.",
        ),
        tokens::Refusal::Invalid | tokens::Refusal::Unsupported => invalid(),
    })?;
    let account = token
        .account
        .as_ref()
        .and_then(|name| accounts?.get(name))
        .ok_or_else(invalid)?;
    Ok(SiteCaller {
        user: Some(account.name.clone()),
        server_admin: false,
        accounts_configured: true,
        token: Some(SiteToken {
            id: token.id.clone(),
            name: token.name.clone(),
            grants: token.grants.clone(),
            expires: token.expires,
            account_server_admin: account.server_admin,
        }),
    })
}

async fn site_middleware(
    State(site): State<Arc<SiteState>>,
    mut request: Request,
    next: axum::middleware::Next,
) -> Response {
    match resolve_caller(&site, request.headers(), auth::now()) {
        Ok(caller) => {
            request.extensions_mut().insert(caller);
            next.run(request).await
        }
        Err(error) => error.into_response(),
    }
}

/// The site's own `/api` routes that the OpenAPI description covers (#330),
/// each registered from its handler's `#[utoipa::path]`.
pub(super) fn site_api() -> OpenApiRouter<Arc<SiteState>> {
    OpenApiRouter::new()
        .routes(routes!(super::routes::health))
        .routes(routes!(super::openapi::openapi_json))
        .routes(routes!(me))
        .routes(routes!(login))
        .routes(routes!(logout))
        .routes(routes!(redeem_invite))
        .routes(routes!(site_info))
        .routes(routes!(get_site_preferences, put_site_preferences))
        .routes(routes!(site_audit_log))
        .routes(routes!(site_status))
        .routes(routes!(site_processing))
        .routes(routes!(cancel_site_job))
        .routes(routes!(site_memberships))
        .routes(routes!(list_accounts, create_account))
        // Bulk creation (#214): a batch of one-time invite links, or
        // accounts with generated passwords for a workshop. matchit prefers
        // a literal segment, so `bulk` cannot be taken for an account name.
        .routes(routes!(create_bulk_invites))
        .routes(routes!(create_bulk_passwords))
        .routes(routes!(update_account, delete_account))
        .routes(routes!(reissue_account_invite))
        .routes(routes!(list_tokens, create_token))
        .routes(routes!(revoke_token))
        .routes(routes!(list_projects, create_project))
        .routes(routes!(project_info, update_project, delete_project))
        .routes(routes!(archive_project))
        .routes(routes!(unarchive_project))
        // Membership and access are project-scoped but administered through
        // the site, because they name site accounts. Registered here so
        // they win over the fallback that delegates everything else to the
        // project's own router.
        .routes(routes!(list_members, add_member))
        .routes(routes!(update_member, remove_member))
        // A project administrator may create an account for their own
        // project, but only through these: they can grant nothing beyond
        // the project in their path. Account lifecycle stays at the site
        // level, for a server administrator.
        .routes(routes!(invite_member))
        .routes(routes!(create_project_bulk_invites))
        .routes(routes!(create_project_bulk_passwords))
        .routes(routes!(project_audit_log))
        .routes(routes!(put_project_access))
}

/// Build the site's Axum application.
pub fn build_site_router(site: Arc<SiteState>) -> Router {
    let access_log =
        axum::middleware::from_fn_with_state(Arc::clone(&site), super::telemetry::access_log);
    Router::new()
        .route("/", get(landing))
        .route("/login", get(login_page))
        .route("/invite/{token}", get(invite_page))
        // The site's own settings: the per-user, site-wide preferences and
        // (for a server administrator) the accounts. Project settings stay
        // under the project's key.
        .route("/settings", get(site_settings_page))
        .merge(Router::from(site_api()))
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
        // Outermost and last, so every request is in the access log (#282),
        // assets and refusals included.
        .layer(access_log)
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
    // A token reaches only its granted projects, and is told so in the same
    // words whether or not the project exists (#194). In one it is granted,
    // its account's standing decides the rest, server administration
    // included.
    let grant = match &identity.token {
        Some(token) => match token.grant_for(&key) {
            Some(grant) => Some(grant.clone()),
            None => {
                return refuse(ApiError::forbidden(
                    "token_not_granted",
                    "This API token has no grant for this project.",
                ))
            }
        },
        None => None,
    };
    let token_name = identity.token.as_ref().map(|token| token.name.clone());
    let identity = identity.account_view();
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
    // A token whose account has left the project is refused rather than
    // given what the public may see there.
    if grant.is_some() && !identity.server_admin {
        let member = state.project.as_ref().is_some_and(|project| {
            identity.user.as_ref().is_some_and(|user| {
                members::read_for_access(project.documents())
                    .get(user)
                    .is_some()
            })
        });
        if !member {
            return refuse(ApiError::forbidden(
                "token_membership_gone",
                "This API token's account is no longer a member of this project.",
            ));
        }
    }
    let caller = match &site.host {
        Host::Local { .. } => auth::local_caller(state.project.is_some(), site.access.read_only),
        Host::Site(_) => auth::project_caller(
            &auth::SiteIdentity {
                token: token_name.as_deref().zip(grant.as_ref()),
                ..identity.identity()
            },
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
    let projects: Vec<ProjectEntry> = keys
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
) -> Option<ProjectEntry> {
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
    // The project's own words (#285). The short one is a card line; the long
    // one is Markdown, rendered here with raw HTML escaped so the template
    // can mark it safe, and shown behind a disclosure.
    let description = project.description();
    let description_long_html = project
        .description_long()
        .filter(|text| !text.trim().is_empty())
        .map(|text| super::markdown::render(&text));
    Some(ProjectEntry {
        key: key.as_str().to_string(),
        name: project
            .config()
            .project
            .name
            .unwrap_or_else(|| key.as_str().to_string()),
        description,
        description_long_html,
        archived: site.site().ok()?.is_archived(key),
        created_by: project
            .config()
            .project
            .created_by
            .map(|user| user.as_str().to_string()),
        member: member.is_some() || caller.server_admin,
        member_count: members.members.len(),
        radargram_count,
        role,
        download,
        require_auth_to_read: members.require_auth_to_read,
    })
}

/// `GET /api/v1/projects`.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub(super) struct ProjectList {
    /// The projects the caller may see, in key order.
    projects: Vec<ProjectEntry>,
}

/// One project as the caller sees it: an entry of `GET /api/v1/projects`,
/// and the whole of `GET /api/v1/projects/{key}`.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub(super) struct ProjectEntry {
    /// What the project is addressed by, in `/api/v1/projects/{key}/…`.
    /// `ridal gui` serves its one project as `default`.
    key: String,
    /// The display name, or the key when the project has none.
    name: String,
    /// One line, for a card.
    #[schema(required = true)]
    description: Option<String>,
    /// The long description rendered from Markdown to HTML, with raw HTML
    /// in the source escaped. `null` when there is none.
    #[schema(required = true)]
    description_long_html: Option<String>,
    archived: bool,
    /// The account that created the project, if it was created through the
    /// site rather than on the command line.
    #[schema(required = true)]
    created_by: Option<String>,
    /// Whether the caller is a member, or a server administrator, who is
    /// treated as one.
    member: bool,
    member_count: usize,
    /// How many radargrams the last scan found, or `null` if the project
    /// has never been scanned.
    #[schema(required = true)]
    radargram_count: Option<usize>,
    /// The caller's role here. A non-member gets `viewer`, and a server
    /// administrator `admin`.
    role: Role,
    /// The caller's download scope here.
    download: DownloadScope,
    /// Whether reading the project needs a login.
    require_auth_to_read: bool,
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
    let projects: Vec<ProjectEntry> = keys
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

/// The response to signing in, by password (`POST /api/v1/auth/login`) or
/// by redeeming an invite (`POST /api/v1/auth/invite`). The session itself
/// is the `ridal_session` cookie set alongside it.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub(super) struct SignedIn {
    user: String,
    server_admin: bool,
}

/// The body of `POST /api/v1/auth/login`.
#[derive(serde::Deserialize, utoipa::ToSchema)]
pub struct LoginBody {
    name: String,
    password: String,
}

fn password_login_allowed(site: &SiteState) -> Result<(), ApiError> {
    if site.access.allow_password_login {
        return Ok(());
    }
    Err(insecure_transport("a password"))
}

/// Why a credential is refused over plain HTTP on a network address.
fn insecure_transport(what: &str) -> ApiError {
    ApiError::forbidden(
        "insecure_transport",
        format!(
            "This server is bound to a network address and does not terminate TLS, \
             so {what} sent to it would travel in the clear. Reach it through a \
             TLS-terminating reverse proxy, or restart it with --allow-insecure-login \
             to accept that."
        ),
    )
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

/// Sign in with a name and password. Every failure gives the same answer,
/// so that the endpoint cannot be used to find out which accounts exist.
/// Refused on a network-bound server without `--allow-insecure-login`.
#[utoipa::path(
    post,
    path = "/api/v1/auth/login",
    tag = "Signing in",
    request_body = LoginBody,
    responses(
        (status = 200, description = "Signed in. Sets the `ridal_session` cookie.", body = SignedIn),
        (status = 401, description = "The name and password do not match an account.", body = ErrorBody),
        (status = 403, description = "`insecure_transport`: passwords are refused over plain HTTP on a network address.", body = ErrorBody),
        (status = 409, description = "`no_accounts`: the site has no accounts.", body = ErrorBody),
    ),
    extensions(("x-ridal-needs" = json!(Needs::new(Who::Nobody)))),
)]
// One refusal for every failure, and a decoy verification for a name that
// does not exist, so the form cannot be used to enumerate accounts or to
// time which ones exist.
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
        Json(SignedIn {
            user: account.name.as_str().to_string(),
            server_admin: account.server_admin,
        }),
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

/// Sign out. Succeeds whether or not anyone was signed in.
#[utoipa::path(
    post,
    path = "/api/v1/auth/logout",
    tag = "Signing in",
    responses((status = 200, description = "Signed out. Clears the cookie.", body = SignedOut)),
    extensions(("x-ridal-needs" = json!(Needs::new(Who::Nobody)))),
)]
async fn logout() -> impl IntoResponse {
    (
        StatusCode::OK,
        [(header::SET_COOKIE, auth::cleared_cookie())],
        Json(SignedOut { signed_out: true }),
    )
}

/// `POST /api/v1/auth/logout`.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub(super) struct SignedOut {
    /// Always `true`. Signing out clears the cookie whether or not there
    /// was a session.
    signed_out: bool,
}

/// Who the site thinks is calling: their name, whether they are signed in,
/// whether they are a server administrator, and whether the site has
/// accounts at all. With an API token, also the token's id, name, grants and
/// expiry.
#[utoipa::path(
    get,
    path = "/api/v1/auth/me",
    tag = "Signing in",
    responses((status = 200, description = "The caller.", body = Me)),
    extensions(("x-ridal-needs" = json!(Needs::new(Who::Nobody)))),
)]
// The one site route that answers for an API token as well as a session,
// so a script can check what its token is (#194).
async fn me(
    State(site): State<Arc<SiteState>>,
    TokenOrSession(caller): TokenOrSession,
) -> impl IntoResponse {
    Json(Me {
        user: caller.user.as_ref().map(|u| u.as_str().to_string()),
        authenticated: caller.user.is_some(),
        server_admin: caller.server_admin,
        authentication_configured: site.accounts_configured(),
        token: caller.token.as_ref().map(|token| TokenSummary {
            id: token.id.clone(),
            name: token.name.clone(),
            grants: token.grants.clone(),
            expires: token.expires.map(rfc3339),
        }),
    })
}

/// `GET /api/v1/auth/me`: who the caller is.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub(super) struct Me {
    /// The signed-in account, or `null` for an anonymous caller. With an API
    /// token, the token's account.
    #[schema(required = true)]
    user: Option<String>,
    authenticated: bool,
    /// Whether the caller administers the site. Always `false` with an API
    /// token, which never carries server administration.
    server_admin: bool,
    /// Whether the site has accounts at all. Without them there is nothing
    /// to sign in to, and every caller is anonymous.
    authentication_configured: bool,
    /// The API token the request came with, or `null` for a session.
    #[schema(required = true)]
    token: Option<TokenSummary>,
}

/// An API token as its holder may see it: never its secret (#194).
#[derive(serde::Serialize, utoipa::ToSchema)]
pub(super) struct TokenSummary {
    /// Names the token in lists and the audit log, and revokes it.
    id: String,
    name: String,
    /// The projects the token may act in, and its ceiling in each. In each,
    /// the account's own membership also applies.
    grants: Vec<tokens::Grant>,
    /// RFC 3339, or `null` for a token that never expires.
    #[schema(required = true)]
    expires: Option<String>,
}

// ---------------------------------------------------------------------------
// API tokens (#194)
// ---------------------------------------------------------------------------

/// The signed-in account behind a token route. Every one of them needs a
/// session: [`SiteCaller`] has already refused a token.
fn token_owner<'a>(
    site: &SiteState,
    caller: &'a SiteCaller,
    action: &str,
) -> Result<&'a UserId, ApiError> {
    if !site.accounts_configured() {
        return Err(ApiError::conflict(
            "no_accounts",
            "This server has no accounts, so there is nobody for a token to act as.",
        ));
    }
    caller.user.as_ref().ok_or_else(|| {
        ApiError::unauthorized("authentication_required", format!("Sign in to {action}."))
    })
}

fn token_error(error: tokens::TokenError) -> ApiError {
    match &error {
        tokens::TokenError::Store(e) => ApiError::internal("store_failed", e.to_string()),
        tokens::TokenError::Malformed { .. } => {
            ApiError::internal("malformed_tokens", error.to_string())
        }
        tokens::TokenError::NotFound(_) => {
            ApiError::not_found("token_not_found", error.to_string())
        }
        tokens::TokenError::Rejected(_) => ApiError::bad_request("rejected", error.to_string()),
    }
}

/// A stored token as the API shows it: everything but the hash.
fn token_info(token: &tokens::Token) -> TokenInfo {
    TokenInfo {
        id: token.id.clone(),
        name: token.name.clone(),
        account: token.account.as_ref().map(|name| name.as_str().to_string()),
        grants: token.grants.clone(),
        created: rfc3339(token.created),
        expires: token.expires.map(rfc3339),
    }
}

/// A stored API token, without its secret or its hash.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub(super) struct TokenInfo {
    id: String,
    name: String,
    /// The account the token acts as.
    #[schema(required = true)]
    account: Option<String>,
    /// The projects the token may act in, and its ceiling in each.
    grants: Vec<tokens::Grant>,
    /// RFC 3339.
    created: String,
    /// RFC 3339, or `null` for a token that never expires.
    #[schema(required = true)]
    expires: Option<String>,
}

/// `GET /api/v1/tokens`.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub(super) struct TokenList {
    tokens: Vec<TokenInfo>,
}

#[derive(serde::Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct TokenListQuery {
    /// Every account's tokens, for a server administrator.
    #[serde(default)]
    all: bool,
}

/// The caller's API tokens, without their secrets. `all=true` lists every
/// account's, for a server administrator.
#[utoipa::path(
    get,
    path = "/api/v1/tokens",
    tag = "Site server",
    params(TokenListQuery),
    responses(
        (status = 200, description = "The tokens.", body = TokenList),
    ),
    extensions(("x-ridal-needs" = json!(Needs::new(Who::SignedIn)))),
)]
// account's, for a server administrator.
async fn list_tokens(
    State(site): State<Arc<SiteState>>,
    caller: SiteCaller,
    axum::extract::Query(query): axum::extract::Query<TokenListQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let owner = token_owner(&site, &caller, "list your API tokens")?;
    if query.all {
        caller.require_server_admin("list every account's API tokens")?;
    }
    let (set, _) = tokens::read(site.site()?.store()).map_err(token_error)?;
    let tokens = set
        .tokens
        .iter()
        .filter(|token| query.all || token.account.as_ref() == Some(owner))
        .map(token_info)
        .collect();
    Ok(Json(TokenList { tokens }))
}

/// The body of `POST /api/v1/tokens`.
#[derive(serde::Deserialize, utoipa::ToSchema)]
pub struct CreateTokenBody {
    /// A label, such as `laptop` or `ci`.
    name: String,
    /// `30d`, `12w`, `2y` or `never`. Defaults to 90 days.
    #[serde(default)]
    expires: Option<String>,
    /// The projects the token may act in. Each grant's role and download
    /// scope may not exceed the account's membership there.
    grants: Vec<tokens::Grant>,
}

/// `POST /api/v1/tokens`: the token, shown this once, and its record.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub(super) struct NewToken {
    /// The whole token, `ridal_<id>_<secret>`. It is not stored and cannot
    /// be shown again; send it as `Authorization: Bearer <token>`.
    token: String,
    info: TokenInfo,
}

/// Create an API token: a `name`, an `expires` (`30d`, `12w`, `2y` or `never`;
/// default 90 days) and `grants`, each a `project`, `role` and `download` no
/// higher than the caller's membership there. The token is in the response once
/// and cannot be shown again. Needs a session, never a token.
#[utoipa::path(
    post,
    path = "/api/v1/tokens",
    tag = "Site server",
    request_body = CreateTokenBody,
    responses(
        (status = 201, description = "The token, shown this once.", body = NewToken),
        (status = 400, description = "A grant above the caller's membership, or a name or expiry that is not valid.", body = ErrorBody),
    ),
    extensions(("x-ridal-needs" = json!(Needs::new(Who::SignedIn)))),
)]
async fn create_token(
    State(site): State<Arc<SiteState>>,
    caller: SiteCaller,
    Json(body): Json<CreateTokenBody>,
) -> Result<impl IntoResponse, ApiError> {
    let owner = token_owner(&site, &caller, "create an API token")?.clone();
    let directory = site.site()?;
    let set = account_set(&site)?;
    let account = set.get(&owner).ok_or_else(|| {
        ApiError::unauthorized("authentication_required", "Sign in to create an API token.")
    })?;
    tokens::check_grants(directory, account, &body.grants).map_err(token_error)?;
    let lifetime =
        tokens::parse_lifetime(body.expires.as_deref().unwrap_or(tokens::DEFAULT_LIFETIME))
            .map_err(token_error)?;
    let (text, token) = tokens::mint(
        auth::now(),
        owner.clone(),
        &body.name,
        body.grants,
        lifetime,
    )
    .map_err(token_error)?;
    tokens::update(directory.store(), |set| {
        set.tokens.push(token.clone());
        Ok(())
    })
    .map_err(token_error)?;
    audit(
        &site,
        site_audit::Entry::new(
            actor(&caller),
            site_audit::Action::TokenCreated,
            owner.as_str(),
        )
        .note(token.describe()),
    );
    Ok((
        StatusCode::CREATED,
        Json(NewToken {
            token: text,
            info: token_info(&token),
        }),
    ))
}

/// Revoke one of the caller's tokens, or any token for a server administrator.
/// Someone else's token is answered as if it did not exist.
#[utoipa::path(
    delete,
    path = "/api/v1/tokens/{id}",
    tag = "Site server",
    responses(
        (status = 204, description = "Revoked."),
        (status = 404, description = "No such token of the caller's.", body = ErrorBody),
    ),
    extensions(("x-ridal-needs" = json!(Needs::new(Who::SignedIn).own()))),
)]
// anyone's for a server administrator.
//
// Someone else's token is not found rather than forbidden, so ids cannot
// be probed.
async fn revoke_token(
    State(site): State<Arc<SiteState>>,
    caller: SiteCaller,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let owner = token_owner(&site, &caller, "revoke an API token")?;
    let directory = site.site()?;
    let revoked = tokens::update(directory.store(), |set| {
        let index = set
            .tokens
            .iter()
            .position(|token| {
                token.id == id && (caller.server_admin || token.account.as_ref() == Some(owner))
            })
            .ok_or_else(|| tokens::TokenError::NotFound(id.clone()))?;
        Ok(set.tokens.remove(index))
    })
    .map_err(token_error)?;
    audit(
        &site,
        site_audit::Entry::new(
            actor(&caller),
            site_audit::Action::TokenRevoked,
            revoked.account.as_ref().map_or("", |name| name.as_str()),
        )
        .note(revoked.describe()),
    );
    Ok(StatusCode::NO_CONTENT)
}

/// Unix seconds as RFC 3339, UTC.
fn rfc3339(seconds: i64) -> String {
    chrono::DateTime::from_timestamp(seconds, 0)
        .map(|at| at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        .unwrap_or_default()
}

fn account_for_invite<'a>(set: &'a AccountSet, token: &str, now: i64) -> Option<&'a Account> {
    set.users.iter().find(|account| {
        account
            .invite
            .as_ref()
            .is_some_and(|invite| invite.is_valid_at(now) && invite::token_matches(invite, token))
    })
}

/// The body of `POST /api/v1/auth/invite`.
#[derive(serde::Deserialize, utoipa::ToSchema)]
pub struct RedeemBody {
    /// The one-time token from the invite link.
    token: String,
    /// The password to set.
    password: String,
}

/// Set a password with a one-time invite token, and sign in. Ends every
/// other session of that account, and adds the membership the invite
/// carried, if it named a project.
#[utoipa::path(
    post,
    path = "/api/v1/auth/invite",
    tag = "Signing in",
    request_body = RedeemBody,
    responses(
        (status = 200, description = "The password is set and the caller signed in. Sets the `ridal_session` cookie.", body = SignedIn),
        (status = 400, description = "`invalid_invite`, or a password that is too weak.", body = ErrorBody),
        (status = 403, description = "`insecure_transport`: passwords are refused over plain HTTP on a network address.", body = ErrorBody),
    ),
    extensions(("x-ridal-needs" = json!(Needs::new(Who::Nobody)))),
)]
// Redeeming consumes the token, sets the password and bumps the credential
// version.
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
        Json(SignedIn {
            user: account.name.as_str().to_string(),
            server_admin: account.server_admin,
        }),
    ))
}

// ---------------------------------------------------------------------------
// Projects API
// ---------------------------------------------------------------------------

/// The site's name, whether it has accounts, and who the caller is.
#[utoipa::path(
    get,
    path = "/api/v1/site",
    tag = "Site server",
    responses(
        (status = 200, description = "The site.", body = SiteInfo),
    ),
    extensions(("x-ridal-needs" = json!(Needs::new(Who::Nobody)))),
)]
async fn site_info(
    State(site): State<Arc<SiteState>>,
    caller: SiteCaller,
) -> Result<impl IntoResponse, ApiError> {
    let configured = site.accounts_configured();
    Ok(Json(SiteInfo {
        name: site.name(),
        accounts_configured: configured,
        authenticated: caller.user.is_some(),
        user: caller.user.as_ref().map(|u| u.as_str().to_string()),
        server_admin: caller.server_admin,
        roles: Role::ALL.to_vec(),
        download_scopes: DownloadScope::ALL.to_vec(),
        invite_ttl_days: invite::INVITE_TTL_DAYS,
        min_password_len: accounts::MIN_PASSWORD_LEN,
    }))
}

/// How much history one request returns. The log keeps roughly the last
/// two [`crate::project::jsonl::MAX_BYTES`]; a page shows the recent end of it.
const AUDIT_PAGE: usize = 500;

/// The site's account, membership and project history, most recent first.
#[utoipa::path(
    get,
    path = "/api/v1/site/audit",
    tag = "Site server",
    responses(
        (status = 200, description = "The history.", body = AuditLog),
    ),
    extensions(("x-ridal-needs" = json!(Needs::new(Who::ServerAdmin)))),
)]
// server administrator.
async fn site_audit_log(
    State(site): State<Arc<SiteState>>,
    caller: SiteCaller,
) -> Result<impl IntoResponse, ApiError> {
    site.require_server_admin(&caller, "read the history")?;
    let log = site_audit::read(site.site()?.store())
        .map_err(|e| ApiError::internal("audit_read_failed", e.to_string()))?;
    let entries: Vec<&site_audit::Entry> = log.entries.iter().rev().take(AUDIT_PAGE).collect();
    Ok(Json(AuditLog {
        entries: entries.into_iter().cloned().collect(),
    }))
}

/// The processing queue: the configured limits, every job it still knows of
/// with its project, owner, state and how long it waited and ran, and how many
/// are queued and running and how much of the memory budget they use.
#[utoipa::path(
    get,
    path = "/api/v1/site/processing",
    tag = "Site server",
    responses(
        (status = 200, description = "The queue.", body = ProcessingQueue),
    ),
    extensions(("x-ridal-needs" = json!(Needs::new(Who::ServerAdmin)))),
)]
// for whoever runs the server (#382): every job it still knows of, with
// its owner and state, and how much of the limits is in use.
async fn site_processing(
    State(site): State<Arc<SiteState>>,
    caller: SiteCaller,
) -> Result<impl IntoResponse, ApiError> {
    site.require_server_admin(&caller, "see the processing queue")?;
    site.site()?;
    let config = site.processing.config();
    const MB: u64 = 1024 * 1024;
    Ok(Json(ProcessingQueue {
        snapshot: site.processing.snapshot(),
        // What the flags set, so the page shows them rather than knowing them.
        limits: ProcessingLimits {
            jobs: config.jobs,
            memory_mb: config.memory_bytes / MB,
            threads_per_job: config.threads_per_job,
            timeout_seconds: config.timeout.as_secs(),
            max_queued_per_user: config.max_queued_per_user,
            max_queued_per_project: config.max_queued_per_project,
            keep_finished_seconds: config.keep_finished.as_secs(),
        },
        jobs: site.processing.details(),
    }))
}

/// What the server's heartbeat would report now: render workers busy and
/// waiting, the render cache, requests and processing since the last heartbeat,
/// and the process's memory and CPU time. Reading it does not reset the
/// heartbeat's counts.
#[utoipa::path(
    get,
    path = "/api/v1/site/status",
    tag = "Site server",
    responses(
        (status = 200, description = "The numbers.", body = super::telemetry::Heartbeat),
    ),
    extensions(("x-ridal-needs" = json!(Needs::new(Who::ServerAdmin)))),
)]
// #391), for a server administrator. Read without starting the
// heartbeat's interval counts again, so looking does not change the log.
async fn site_status(
    State(site): State<Arc<SiteState>>,
    caller: SiteCaller,
) -> Result<impl IntoResponse, ApiError> {
    site.require_server_admin(&caller, "see the server's status")?;
    Ok(Json(super::telemetry::Heartbeat::peek(&site)))
}

/// Cancel anyone's processing job: a queued one never starts, a running one is
/// stopped. Recorded in the site's history.
#[utoipa::path(
    delete,
    path = "/api/v1/site/processing/{job}",
    tag = "Site server",
    responses(
        (status = 204, description = "Cancelled."),
        (status = 404, description = "`job_not_found`: it has already ended, or does not exist.", body = ErrorBody),
    ),
    extensions(("x-ridal-needs" = json!(Needs::new(Who::ServerAdmin)))),
)]
// anyone's job (#391): a queued one never starts, a running one is killed.
// The batch or draft it belonged to records it as cancelled.
async fn cancel_site_job(
    State(site): State<Arc<SiteState>>,
    caller: SiteCaller,
    Path(job): Path<u64>,
) -> Result<impl IntoResponse, ApiError> {
    site.require_server_admin(&caller, "cancel a processing job")?;
    let gone = || {
        ApiError::not_found(
            "job_not_found",
            format!("There is no queued or running processing job {job}."),
        )
    };
    let detail = site.processing.detail(job).ok_or_else(gone)?;
    if !site.processing.cancel(job) {
        return Err(gone());
    }
    let mut entry = site_audit::Entry::new(
        actor(&caller),
        site_audit::Action::ProcessingJobCancelled,
        detail.status.owner.as_str(),
    )
    .note(format!(
        "job {job}, radargram {}",
        detail.status.radargram_id
    ));
    entry.project = ProjectKey::new(&detail.project).ok();
    audit(&site, entry);
    Ok(StatusCode::NO_CONTENT)
}

/// This project's slice of the site history.
#[utoipa::path(
    get,
    path = "/api/v1/projects/{key}/audit",
    tag = "Site server",
    responses(
        (status = 200, description = "The history.", body = AuditLog),
        (status = 404, description = "No such project, or one the caller may not see.", body = ErrorBody),
    ),
    extensions(("x-ridal-needs" = json!(Needs::new(Who::Role(Role::Admin))))),
)]
// people who administer it. Only the entries that name this project.
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
    Ok(Json(AuditLog {
        entries: entries.into_iter().cloned().collect(),
    }))
}

/// The body of `PUT /api/v1/site/preferences`.
#[derive(serde::Deserialize, utoipa::ToSchema)]
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

/// The caller's own site-wide settings, currently the theme.
#[utoipa::path(
    get,
    path = "/api/v1/site/preferences",
    tag = "Site server",
    responses(
        (status = 200, description = "Their settings.", body = SitePreferences),
    ),
    extensions(("x-ridal-needs" = json!(Needs::new(Who::SignedIn)))),
)]
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
    Ok(Json(SitePreferences {
        user: user.as_str().to_string(),
        theme: preferences.theme,
    }))
}

/// Change the caller's own site-wide settings. Any role may.
#[utoipa::path(
    put,
    path = "/api/v1/site/preferences",
    tag = "Site server",
    request_body = SitePreferencesBody,
    responses(
        (status = 200, description = "Their settings as saved.", body = SitePreferences),
        (status = 400, description = "`unknown_theme`.", body = ErrorBody),
    ),
    extensions(("x-ridal-needs" = json!(Needs::new(Who::SignedIn)))),
)]
// A viewer may do this, exactly as with project preferences: choosing how
// you like to look at something is the floor of having an account.
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
    Ok(Json(SitePreferences {
        user: user.as_str().to_string(),
        theme: stored.theme,
    }))
}

/// The projects the caller is a member of; every project for a server
/// administrator. Public projects are not listed. With an API token, only its
/// granted projects, with the role and download scope the token has there.
#[utoipa::path(
    get,
    path = "/api/v1/projects",
    tag = "Site server",
    responses(
        (status = 200, description = "The projects.", body = ProjectList),
    ),
    extensions(("x-ridal-needs" = json!(Needs::new(Who::Nobody)))),
)]
async fn list_projects(
    State(site): State<Arc<SiteState>>,
    TokenOrSession(caller): TokenOrSession,
) -> Result<impl IntoResponse, ApiError> {
    let keys = site.site()?.list().map_err(site_error)?;
    let projects: Vec<ProjectEntry> = keys
        .iter()
        .filter_map(|key| reachable_entry(&site, key, &caller, Lookup::Listing))
        .collect();
    Ok(Json(ProjectList { projects }))
}

/// A project as `caller` may reach it: for a token, only a granted project,
/// with the role and download scope the token actually has there (#194).
fn reachable_entry(
    site: &SiteState,
    key: &ProjectKey,
    caller: &SiteCaller,
    lookup: Lookup,
) -> Option<ProjectEntry> {
    let Some(token) = &caller.token else {
        return project_entry(site, key, caller, lookup);
    };
    let grant = token.grant_for(key)?;
    let mut entry = project_entry(site, key, &caller.account_view(), lookup)?;
    entry.role = entry.role.min(grant.role);
    entry.download = entry.download.min(grant.download);
    Some(entry)
}

/// The body of `POST /api/v1/projects`.
#[derive(serde::Deserialize, utoipa::ToSchema)]
pub struct CreateProjectBody {
    /// The project's key: a slug, its directory and its place in every URL.
    key: String,
    /// Its display name.
    #[serde(default)]
    name: Option<String>,
}

/// Create a project at a key.
#[utoipa::path(
    post,
    path = "/api/v1/projects",
    tag = "Site server",
    request_body = CreateProjectBody,
    responses(
        (status = 201, description = "Created.", body = CreatedProject),
        (status = 400, description = "A key that is not valid.", body = ErrorBody),
        (status = 409, description = "A project with that key exists.", body = ErrorBody),
        (status = 403, description = "`read_only`: the server was started read-only.", body = ErrorBody),
    ),
    extensions(("x-ridal-needs" = json!(Needs::new(Who::ServerAdmin)))),
)]
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
        Json(CreatedProject {
            key: key.as_str().to_string(),
            name: project.config().project.name.clone(),
        }),
    ))
}

/// One project, with the caller's role and download scope in it. A project
/// the caller may not see, signed in or not, answers `404` as one that does
/// not exist.
#[utoipa::path(
    get,
    path = "/api/v1/projects/{key}",
    tag = "Site server",
    responses(
        (status = 200, description = "The project.", body = ProjectEntry),
        (status = 404, description = "No such project, or one the caller may not see.", body = ErrorBody),
    ),
    extensions(("x-ridal-needs" = json!(Needs::new(Who::Anyone)))),
)]
async fn project_info(
    State(site): State<Arc<SiteState>>,
    TokenOrSession(caller): TokenOrSession,
    Path(key): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let key = parse_key(&key)?;
    reachable_entry(&site, &key, &caller, Lookup::ByKey)
        .map(Json)
        .ok_or_else(hidden_project)
}

/// The body of `PATCH /api/v1/projects/{key}`.
#[derive(serde::Deserialize, utoipa::ToSchema)]
pub struct UpdateProjectBody {
    /// The new display name; `null` or empty removes it.
    #[serde(default)]
    name: Option<String>,
}

/// Rename its display name; the key does not change.
#[utoipa::path(
    patch,
    path = "/api/v1/projects/{key}",
    tag = "Site server",
    request_body = UpdateProjectBody,
    responses(
        (status = 200, description = "Renamed.", body = RenamedProject),
        (status = 404, description = "No such project, or one the caller may not see.", body = ErrorBody),
        (status = 403, description = "`read_only`: the server was started read-only.", body = ErrorBody),
    ),
    extensions(("x-ridal-needs" = json!(Needs::new(Who::ServerAdmin)))),
)]
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
    Ok(Json(RenamedProject {
        key: key.as_str().to_string(),
    }))
}

/// Make the project read-only.
#[utoipa::path(
    post,
    path = "/api/v1/projects/{key}/archive",
    tag = "Site server",
    responses(
        (status = 200, description = "Archived.", body = ArchivedProject),
        (status = 404, description = "No such project, or one the caller may not see.", body = ErrorBody),
        (status = 403, description = "`read_only`: the server was started read-only.", body = ErrorBody),
    ),
    extensions(("x-ridal-needs" = json!(Needs::new(Who::ServerAdmin)))),
)]
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
    Ok(Json(ArchivedProject {
        key: key.as_str().to_string(),
        archived: true,
    }))
}

/// Reverse an archive.
#[utoipa::path(
    post,
    path = "/api/v1/projects/{key}/unarchive",
    tag = "Site server",
    responses(
        (status = 200, description = "No longer archived.", body = ArchivedProject),
        (status = 404, description = "No such project, or one the caller may not see.", body = ErrorBody),
        (status = 403, description = "`read_only`: the server was started read-only.", body = ErrorBody),
    ),
    extensions(("x-ridal-needs" = json!(Needs::new(Who::ServerAdmin)))),
)]
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
    Ok(Json(ArchivedProject {
        key: key.as_str().to_string(),
        archived: false,
    }))
}

/// Delete an archived project and everything it owns.
#[utoipa::path(
    delete,
    path = "/api/v1/projects/{key}",
    tag = "Site server",
    responses(
        (status = 204, description = "Deleted."),
        (status = 404, description = "No such project, or one the caller may not see.", body = ErrorBody),
        (status = 409, description = "`not_archived`: archive it first.", body = ErrorBody),
        (status = 403, description = "`read_only`: the server was started read-only.", body = ErrorBody),
    ),
    extensions(("x-ridal-needs" = json!(Needs::new(Who::ServerAdmin)))),
)]
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

fn member_json(member: &members::Member) -> MemberView {
    MemberView {
        name: member.name.as_str().to_string(),
        role: member.role,
        download: member.download,
    }
}

/// The membership list and access policy.
#[utoipa::path(
    get,
    path = "/api/v1/projects/{key}/members",
    tag = "Site server",
    responses(
        (status = 200, description = "The members.", body = MemberList),
        (status = 404, description = "No such project, or one the caller may not see.", body = ErrorBody),
    ),
    extensions(("x-ridal-needs" = json!(Needs::new(Who::Role(Role::Admin))))),
)]
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
    Ok(Json(MemberList {
        members: set.members.iter().map(member_json).collect(),
        require_auth_to_read: set.require_auth_to_read,
        anonymous_download: set.anonymous_download,
        configured,
        has_admin: set.has_admin(),
        roles: Role::ALL.to_vec(),
        download_scopes: DownloadScope::ALL.to_vec(),
    }))
}

/// The body of `POST /api/v1/projects/{key}/members` and
/// `…/members/invite`.
#[derive(serde::Deserialize, utoipa::ToSchema)]
pub struct MemberBody {
    /// The account.
    name: String,
    /// `viewer`, `picker`, `operator` or `admin`.
    role: String,
    /// The download scope; `all` when left out.
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

/// Add or update an existing account's membership.
#[utoipa::path(
    post,
    path = "/api/v1/projects/{key}/members",
    tag = "Site server",
    request_body = MemberBody,
    responses(
        (status = 201, description = "The membership.", body = MemberView),
        (status = 400, description = "A role or scope that is not valid.", body = ErrorBody),
        (status = 404, description = "`account_not_found`, or no such project.", body = ErrorBody),
        (status = 403, description = "`archived`: the project is archived.", body = ErrorBody),
    ),
    extensions(("x-ridal-needs" = json!(Needs::new(Who::Role(Role::Admin))))),
)]
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
        Json(MemberView {
            name: name.as_str().to_string(),
            role,
            download,
        }),
    ))
}

/// Create an account and invite it into this project only.
#[utoipa::path(
    post,
    path = "/api/v1/projects/{key}/members/invite",
    tag = "Site server",
    request_body = MemberBody,
    responses(
        (status = 201, description = "Created, with its invite link.", body = InvitedMember),
        (status = 400, description = "A name, role or scope that is not valid.", body = ErrorBody),
        (status = 404, description = "No such project, or one the caller may not see.", body = ErrorBody),
        (status = 409, description = "`name_in_use`, `account_exists` or `duplicate`: the name is taken.", body = ErrorBody),
        (status = 403, description = "`archived`: the project is archived.", body = ErrorBody),
    ),
    extensions(("x-ridal-needs" = json!(Needs::new(Who::Role(Role::Admin))))),
)]
// invite it into this project.
//
// A project administrator may create an account, but only for their own
// project: the invite carries this project, role and download and nothing
// else. The account is never a server administrator, and account lifecycle
// -- deleting it, resetting its password, promoting it -- stays with a
// server administrator at the site level.
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
        Json(InvitedMember {
            name: name.as_str().to_string(),
            role,
            download,
            invite_path: format!("/invite/{token}"),
            invite_expires: invite.expires,
            invite_ttl_days: invite::INVITE_TTL_DAYS,
        }),
    ))
}

/// The body of `PUT /api/v1/projects/{key}/members/{name}`. A field left
/// out is not changed.
#[derive(serde::Deserialize, utoipa::ToSchema)]
pub struct UpdateMemberBody {
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    download: Option<String>,
}

/// Change a member's role or download scope. Refused for the project's last
/// administrator.
#[utoipa::path(
    put,
    path = "/api/v1/projects/{key}/members/{name}",
    tag = "Site server",
    request_body = UpdateMemberBody,
    responses(
        (status = 200, description = "The membership.", body = MemberView),
        (status = 400, description = "A role or scope that is not valid, or the last administrator.", body = ErrorBody),
        (status = 404, description = "No such member, or no such project.", body = ErrorBody),
        (status = 403, description = "`archived`: the project is archived.", body = ErrorBody),
    ),
    extensions(("x-ridal-needs" = json!(Needs::new(Who::Role(Role::Admin))))),
)]
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

/// Remove the membership. The account itself remains. Refused for the project's
/// last administrator.
#[utoipa::path(
    delete,
    path = "/api/v1/projects/{key}/members/{name}",
    tag = "Site server",
    responses(
        (status = 204, description = "Removed."),
        (status = 400, description = "The last administrator.", body = ErrorBody),
        (status = 404, description = "No such project, or one the caller may not see.", body = ErrorBody),
        (status = 403, description = "`archived`: the project is archived.", body = ErrorBody),
    ),
    extensions(("x-ridal-needs" = json!(Needs::new(Who::Role(Role::Admin))))),
)]
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

/// The body of `PUT /api/v1/projects/{key}/access`. A field left out is not
/// changed.
#[derive(serde::Deserialize, utoipa::ToSchema)]
pub struct AccessBody {
    /// Whether reading the project needs a login.
    #[serde(default)]
    require_auth_to_read: Option<bool>,
    /// What someone not signed in may download.
    #[serde(default)]
    anonymous_download: Option<String>,
}

/// The project's read and anonymous-download policy.
#[utoipa::path(
    put,
    path = "/api/v1/projects/{key}/access",
    tag = "Site server",
    request_body = AccessBody,
    responses(
        (status = 200, description = "The policy as saved.", body = AccessPolicy),
        (status = 400, description = "A scope that is not valid.", body = ErrorBody),
        (status = 404, description = "No such project, or one the caller may not see.", body = ErrorBody),
        (status = 403, description = "`archived`: the project is archived.", body = ErrorBody),
    ),
    extensions(("x-ridal-needs" = json!(Needs::new(Who::Role(Role::Admin))))),
)]
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

    Ok(Json(AccessPolicy {
        require_auth_to_read: set.require_auth_to_read,
        anonymous_download: set.anonymous_download,
    }))
}

// ---------------------------------------------------------------------------
// Accounts API
// ---------------------------------------------------------------------------

/// Every account, without hashes or invite tokens.
#[utoipa::path(
    get,
    path = "/api/v1/accounts",
    tag = "Site server",
    responses(
        (status = 200, description = "The accounts.", body = AccountList),
    ),
    extensions(("x-ridal-needs" = json!(Needs::new(Who::ServerAdmin)))),
)]
async fn list_accounts(
    State(site): State<Arc<SiteState>>,
    caller: SiteCaller,
) -> Result<impl IntoResponse, ApiError> {
    site.require_server_admin(&caller, "see the accounts")?;
    let set = account_set(&site)?;
    Ok(Json(AccountList {
        accounts: set.users.iter().map(Account::redacted).collect(),
        invite_ttl_days: invite::INVITE_TTL_DAYS,
        min_password_len: accounts::MIN_PASSWORD_LEN,
        account_file: ACCOUNTS_FILE.to_string(),
    }))
}

/// Every account and the projects it belongs to.
#[utoipa::path(
    get,
    path = "/api/v1/site/memberships",
    tag = "Site server",
    responses(
        (status = 200, description = "The memberships.", body = MembershipOverview),
    ),
    extensions(("x-ridal-needs" = json!(Needs::new(Who::ServerAdmin)))),
)]
// belongs to, for a read-only overview on the site settings page.
async fn site_memberships(
    State(site): State<Arc<SiteState>>,
    caller: SiteCaller,
) -> Result<impl IntoResponse, ApiError> {
    site.require_server_admin(&caller, "see memberships")?;
    let accounts = account_set(&site)?;
    let keys = site.site()?.list().map_err(site_error)?;

    // Seeded from the accounts so one with no memberships still appears.
    let mut by_account: std::collections::HashMap<String, Vec<Membership>> = accounts
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
                list.push(Membership {
                    project: key.as_str().to_string(),
                    project_name: name.clone(),
                    role: member.role,
                    download: member.download,
                });
            }
        }
    }

    let overview: Vec<AccountMemberships> = accounts
        .users
        .iter()
        .map(|account| {
            let memberships = by_account.remove(account.name.as_str()).unwrap_or_default();
            AccountMemberships {
                name: account.name.as_str().to_string(),
                memberships,
            }
        })
        .collect();
    Ok(Json(MembershipOverview { accounts: overview }))
}

/// The body of `POST /api/v1/accounts`.
#[derive(serde::Deserialize, utoipa::ToSchema)]
pub struct CreateAccountBody {
    name: String,
    /// Refused when `true`: grant it once the account has a password.
    #[serde(default)]
    server_admin: bool,
    /// When set, the invite grants a membership in this project on
    /// redemption, with `role` and `download`. A server administrator may
    /// instead invite with no project.
    #[serde(default)]
    project: Option<String>,
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    download: Option<String>,
}

/// Create an account and mint its invite, optionally naming a project, role and
/// download scope. It cannot create a server administrator.
#[utoipa::path(
    post,
    path = "/api/v1/accounts",
    tag = "Site server",
    request_body = CreateAccountBody,
    responses(
        (status = 201, description = "Created, with its invite link.", body = CreatedAccount),
        (status = 400, description = "`server_admin_at_creation`, or a name, role or scope that is not valid.", body = ErrorBody),
        (status = 409, description = "`name_in_use`, `account_exists` or `duplicate`: the name is taken.", body = ErrorBody),
        (status = 403, description = "`read_only`: the server was started read-only.", body = ErrorBody),
    ),
    extensions(("x-ridal-needs" = json!(Needs::new(Who::ServerAdmin)))),
)]
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
        Json(CreatedAccount {
            account: created.redacted(),
            invite_path: format!("/invite/{token}"),
            invite_expires: expires,
            invite_ttl_days: invite::INVITE_TTL_DAYS,
        }),
    ))
}

/// The body of `PUT /api/v1/accounts/{name}`.
#[derive(serde::Deserialize, utoipa::ToSchema)]
pub struct UpdateAccountBody {
    /// Grant or revoke server administration.
    #[serde(default)]
    server_admin: Option<bool>,
}

/// Change the server-administrator flag. Refused if it would leave the site
/// with none, or while the account has an invite outstanding.
#[utoipa::path(
    put,
    path = "/api/v1/accounts/{name}",
    tag = "Site server",
    request_body = UpdateAccountBody,
    responses(
        (status = 200, description = "The account.", body = accounts::RedactedAccount),
        (status = 400, description = "Refused: the last server administrator, or an invite outstanding.", body = ErrorBody),
        (status = 404, description = "No such account.", body = ErrorBody),
        (status = 403, description = "`read_only`: the server was started read-only.", body = ErrorBody),
    ),
    extensions(("x-ridal-needs" = json!(Needs::new(Who::ServerAdmin)))),
)]
// Bumps the credential version, so a demotion reaches a live session on its
// next request rather than when the cookie ages out.
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

/// Remove an account and its membership in every project. Picks are left in
/// place.
#[utoipa::path(
    delete,
    path = "/api/v1/accounts/{name}",
    tag = "Site server",
    responses(
        (status = 204, description = "Removed."),
        (status = 404, description = "No such account.", body = ErrorBody),
        (status = 403, description = "`read_only`: the server was started read-only.", body = ErrorBody),
    ),
    extensions(("x-ridal-needs" = json!(Needs::new(Who::ServerAdmin)))),
)]
// Removes the account and its membership in every project, never any
// project's picks. A later account of the same name starts with nothing.
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

/// Issue a new invite link, for a reset or a lost one.
#[utoipa::path(
    post,
    path = "/api/v1/accounts/{name}/invite",
    tag = "Site server",
    responses(
        (status = 200, description = "The new link.", body = ReissuedInvite),
        (status = 404, description = "No such account.", body = ErrorBody),
        (status = 403, description = "`read_only`: the server was started read-only.", body = ErrorBody),
    ),
    extensions(("x-ridal-needs" = json!(Needs::new(Who::ServerAdmin)))),
)]
// link.
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

    Ok(Json(ReissuedInvite {
        user: name.as_str().to_string(),
        invite_path: format!("/invite/{token}"),
        invite_expires: expires,
        invite_ttl_days: invite::INVITE_TTL_DAYS,
    }))
}

/// The body of the bulk invite routes. The bulk password routes take the
/// same, with `acknowledge_risk`.
#[derive(serde::Deserialize, utoipa::ToSchema)]
pub struct BulkAccountsBody {
    /// What the names start with, numbered after it. Ignored when
    /// `random_names` is set.
    #[serde(default)]
    prefix: String,
    /// How many accounts.
    count: usize,
    /// Random names instead of `prefix` and a number.
    #[serde(default)]
    random_names: bool,
    role: String,
    #[serde(default)]
    download: Option<String>,
    /// The project they join: each invite grants this membership on
    /// redemption. A project administrator's route names it in the path
    /// instead.
    #[serde(default)]
    project: Option<String>,
}

/// The body of the bulk password routes.
#[derive(serde::Deserialize, utoipa::ToSchema)]
pub struct BulkPasswordsBody {
    /// What the names start with, numbered after it. Ignored when
    /// `random_names` is set.
    #[serde(default)]
    prefix: String,
    /// How many accounts.
    count: usize,
    /// Random names instead of `prefix` and a number.
    #[serde(default)]
    random_names: bool,
    /// Any role but `admin`.
    role: String,
    #[serde(default)]
    download: Option<String>,
    /// The project they are added to. A project administrator's route
    /// names it in the path instead.
    #[serde(default)]
    project: Option<String>,
    /// Must be `true`: shared passwords are less safe than invite links.
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
) -> Result<(StatusCode, Json<BulkInvites>), ApiError> {
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
        Json(BulkInvites {
            users: minted
                .iter()
                .map(|(name, token, expires)| BulkInvite {
                    name: name.as_str().to_string(),
                    invite_path: format!("/invite/{token}"),
                    invite_expires: *expires,
                })
                .collect(),
            invite_ttl_days: invite::INVITE_TTL_DAYS,
        }),
    ))
}

/// Create several invite-only accounts at once.
#[utoipa::path(
    post,
    path = "/api/v1/accounts/bulk/invites",
    tag = "Site server",
    request_body = BulkAccountsBody,
    responses(
        (status = 201, description = "Created, each with its invite link.", body = BulkInvites),
        (status = 400, description = "A role, scope or count that is not valid.", body = ErrorBody),
        (status = 403, description = "`read_only`: the server was started read-only.", body = ErrorBody),
    ),
    extensions(("x-ridal-needs" = json!(Needs::new(Who::ServerAdmin)))),
)]
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

/// Bulk-create accounts, each granted this project only.
#[utoipa::path(
    post,
    path = "/api/v1/projects/{key}/members/bulk/invites",
    tag = "Site server",
    request_body = BulkAccountsBody,
    responses(
        (status = 201, description = "Created, each with its invite link.", body = BulkInvites),
        (status = 400, description = "A role, scope or count that is not valid.", body = ErrorBody),
        (status = 404, description = "No such project, or one the caller may not see.", body = ErrorBody),
        (status = 403, description = "`archived`: the project is archived.", body = ErrorBody),
    ),
    extensions(("x-ridal-needs" = json!(Needs::new(Who::Role(Role::Admin))))),
)]
// administrator's batch, scoped to their project.
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
) -> Result<(StatusCode, Json<BulkPasswords>), ApiError> {
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
        Json(BulkPasswords {
            users: generated
                .iter()
                .map(|(name, password)| GeneratedPassword {
                    name: name.as_str().to_string(),
                    password: password.clone(),
                })
                .collect(),
            advisory: advisory.to_string(),
        }),
    ))
}

/// Create several accounts with generated passwords.
#[utoipa::path(
    post,
    path = "/api/v1/accounts/bulk/passwords",
    tag = "Site server",
    request_body = BulkPasswordsBody,
    responses(
        (status = 200, description = "Created, each with its password.", body = BulkPasswords),
        (status = 400, description = "`risk_acknowledgement_required`, `admin_bulk_passwords_forbidden`, or a role, scope or count that is not valid.", body = ErrorBody),
        (status = 403, description = "`read_only`: the server was started read-only.", body = ErrorBody),
    ),
    extensions(("x-ridal-needs" = json!(Needs::new(Who::ServerAdmin)))),
)]
// passwords, for a workshop where handing out links is impractical.
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

/// Bulk-create accounts with generated passwords, each granted this project
/// only.
#[utoipa::path(
    post,
    path = "/api/v1/projects/{key}/members/bulk/passwords",
    tag = "Site server",
    request_body = BulkPasswordsBody,
    responses(
        (status = 200, description = "Created, each with its password.", body = BulkPasswords),
        (status = 400, description = "`risk_acknowledgement_required`, `admin_bulk_passwords_forbidden`, or a role, scope or count that is not valid.", body = ErrorBody),
        (status = 404, description = "No such project, or one the caller may not see.", body = ErrorBody),
        (status = 403, description = "`archived`: the project is archived.", body = ErrorBody),
    ),
    extensions(("x-ridal-needs" = json!(Needs::new(Who::Role(Role::Admin))))),
)]
// administrator's batch, scoped to their project.
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

// ---------------------------------------------------------------------------
// Response bodies (#330)
// ---------------------------------------------------------------------------

/// `GET /api/v1/site`: the site, and who the caller is in it.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub(super) struct SiteInfo {
    name: String,
    /// Whether the site has accounts to sign in to.
    accounts_configured: bool,
    authenticated: bool,
    #[schema(required = true)]
    user: Option<String>,
    server_admin: bool,
    /// The project roles, least first.
    roles: Vec<Role>,
    /// The download scopes, least first.
    download_scopes: Vec<DownloadScope>,
    /// How long an invite link is good for.
    invite_ttl_days: i64,
    /// The shortest password an account may set.
    min_password_len: usize,
}

/// A history, most recent first.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub(super) struct AuditLog {
    entries: Vec<site_audit::Entry>,
}

/// `GET /api/v1/site/processing`: the processing queue.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub(super) struct ProcessingQueue {
    /// How many jobs are queued and running, and the memory they use.
    snapshot: super::processing::QueueSnapshot,
    limits: ProcessingLimits,
    /// Every job the queue still knows of.
    jobs: Vec<super::processing::JobDetail>,
}

/// The processing limits the server was started with.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub(super) struct ProcessingLimits {
    jobs: usize,
    memory_mb: u64,
    threads_per_job: usize,
    timeout_seconds: u64,
    max_queued_per_user: usize,
    max_queued_per_project: usize,
    keep_finished_seconds: u64,
}

/// The caller's own site-wide settings.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub(super) struct SitePreferences {
    user: String,
    /// `light` or `dark`; `null` follows the device.
    #[schema(required = true)]
    theme: Option<String>,
}

/// `POST /api/v1/projects`: the project created.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub(super) struct CreatedProject {
    key: String,
    #[schema(required = true)]
    name: Option<String>,
}

/// `PATCH /api/v1/projects/{key}`: the project renamed.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub(super) struct RenamedProject {
    key: String,
}

/// A project archived or unarchived.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub(super) struct ArchivedProject {
    key: String,
    archived: bool,
}

/// One project membership.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub(super) struct MemberView {
    name: String,
    role: Role,
    download: DownloadScope,
}

/// `GET /api/v1/projects/{key}/members`: the members and the access
/// policy.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub(super) struct MemberList {
    members: Vec<MemberView>,
    /// Whether reading the project needs a login.
    require_auth_to_read: bool,
    /// What someone not signed in may download.
    anonymous_download: DownloadScope,
    /// Whether the project has a membership file at all.
    configured: bool,
    /// Whether any member is an `admin`.
    has_admin: bool,
    roles: Vec<Role>,
    download_scopes: Vec<DownloadScope>,
}

/// `POST /api/v1/projects/{key}/members/invite`: the account created, and
/// the link that sets its password.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub(super) struct InvitedMember {
    name: String,
    role: Role,
    download: DownloadScope,
    /// The invite link's path, `/invite/<token>`. It is shown this once.
    invite_path: String,
    /// When it expires, in Unix seconds.
    invite_expires: i64,
    invite_ttl_days: i64,
}

/// `PUT /api/v1/projects/{key}/access`: the access policy as saved.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub(super) struct AccessPolicy {
    require_auth_to_read: bool,
    anonymous_download: DownloadScope,
}

/// `GET /api/v1/accounts`: every account.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub(super) struct AccountList {
    accounts: Vec<accounts::RedactedAccount>,
    invite_ttl_days: i64,
    min_password_len: usize,
    /// The file the accounts are kept in, relative to the site.
    account_file: String,
}

/// `GET /api/v1/site/memberships`: every account and its projects.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub(super) struct MembershipOverview {
    accounts: Vec<AccountMemberships>,
}

/// One account and the projects it belongs to.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub(super) struct AccountMemberships {
    name: String,
    memberships: Vec<Membership>,
}

/// One account's membership in one project.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub(super) struct Membership {
    project: String,
    /// Its display name, or its key when it has none.
    project_name: String,
    role: Role,
    download: DownloadScope,
}

/// `POST /api/v1/accounts`: the account created, and the link that sets its
/// password.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub(super) struct CreatedAccount {
    account: accounts::RedactedAccount,
    /// The invite link's path, `/invite/<token>`. It is shown this once.
    invite_path: String,
    /// When it expires, in Unix seconds.
    invite_expires: i64,
    invite_ttl_days: i64,
}

/// `POST /api/v1/accounts/{name}/invite`: a new invite link.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub(super) struct ReissuedInvite {
    user: String,
    /// The invite link's path, `/invite/<token>`. It is shown this once.
    invite_path: String,
    /// When it expires, in Unix seconds.
    invite_expires: i64,
    invite_ttl_days: i64,
}

/// The accounts a bulk invite created, each with its link.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub(super) struct BulkInvites {
    users: Vec<BulkInvite>,
    invite_ttl_days: i64,
}

/// One account a bulk invite created.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub(super) struct BulkInvite {
    name: String,
    /// The invite link's path, `/invite/<token>`. It is shown this once.
    invite_path: String,
    /// When it expires, in Unix seconds.
    invite_expires: i64,
}

/// The accounts a bulk password request created, each with its password.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub(super) struct BulkPasswords {
    users: Vec<GeneratedPassword>,
    /// What to tell the people who receive them.
    advisory: String,
}

/// One account a bulk password request created. The password is shown this
/// once and not stored.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub(super) struct GeneratedPassword {
    name: String,
    password: String,
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
