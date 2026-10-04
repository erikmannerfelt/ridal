//! Who is asking, and how the server knows (#131).
//!
//! # Sessions are a signed cookie, not a table
//!
//! The stateless option is the smaller one here, which is worth saying
//! because the instinct runs the other way. An in-memory session map loses
//! every login on restart, and avoiding that means writing sessions to disk
//! and sweeping them for expiry. A signed cookie stores nothing
//! server-side, so surviving a restart falls out of the *key* being
//! persistent rather than of persisting sessions.
//!
//! The primitive was already a dependency. `blake3::keyed_hash` is a keyed
//! MAC, and [`blake3::Hash`] provides constant-time equality -- its docs say
//! so, and it deliberately omits `Deref`/`AsRef` so the property cannot be
//! lost to an implicit conversion into a byte slice.
//!
//! # Revocation
//!
//! The wrinkle a stateless cookie brings is that it cannot be withdrawn
//! before it expires, so deleting a user or demoting them would not take
//! effect until their cookie aged out. Each account carries a credential
//! version; the cookie carries the version it was minted at, and
//! verification compares the two against the site's `accounts.json`, which
//! is a small file the server reads anyway. Changing a password or the
//! server-administrator flag bumps it, and a membership is read afresh on
//! every request, so account management takes effect immediately.
//!
//! # Where identity is decided
//!
//! Accounts belong to a site (#214). The site resolves the account behind a
//! request once, works out the project [`Caller`] from it and the project's
//! memberships ([`project_caller`]), and hands that to the project's router
//! in the request extensions. A project never reads an account or a cookie
//! itself. `ridal gui` has no accounts at all: its one person is the local
//! [`DEFAULT_USER`](crate::identity::DEFAULT_USER) ([`local_caller`]).
//!
//! # Transport
//!
//! A password over plain HTTP is cleartext on the wire, and Ridal will
//! essentially never see HTTPS: behind a TLS-terminating proxy it sees plain
//! HTTP on loopback, which is correct and safe. So the guardrail cannot ask
//! "is this connection TLS?" -- the answer is always no. It is keyed on the
//! *bind address* instead, in [`super::launch`].
//!
//! For the same reason the cookie is not marked `Secure`: that attribute
//! would stop it being sent over the loopback HTTP that both `ridal gui` and
//! every reverse-proxy deployment actually speak. `HttpOnly` and
//! `SameSite=Lax` are set, and both are meaningful regardless of transport.

use std::path::Path;
use std::sync::Arc;

use axum::extract::{FromRequestParts, Request, State};
use axum::http::{header, request::Parts, HeaderMap};
use axum::middleware::Next;
use axum::response::{IntoResponse, Redirect, Response};

use super::app::AppState;
use super::routes::ApiError;
use crate::identity::UserId;
use crate::project::members;
use crate::project::roles::{DownloadScope, Role};
use crate::project::store::{DocumentStore, Expectation};
use crate::project::Project;
use crate::site::accounts;

/// Name of the session cookie.
pub const SESSION_COOKIE: &str = "ridal_session";

/// The key file, relative to the site root. Created on first sign-in.
pub const SESSION_KEY_FILE: &str = "session.key";

/// How long a session lasts. Long enough that a working week does not mean
/// five logins, short enough that a forgotten browser on a shared machine
/// does not stay signed in indefinitely.
pub const SESSION_TTL_DAYS: i64 = 14;

/// Domain separator, so a signature minted for a session can never be
/// mistaken for one minted for anything else this key might sign later.
const SESSION_DOMAIN: &str = "ridal-session-v1";

/// The key that signs session cookies.
#[derive(Clone)]
pub struct SessionKey([u8; 32]);

impl std::fmt::Debug for SessionKey {
    /// Never prints the key. A `Debug` that did would put a forgery kit for
    /// every account into any log line that formatted the app state.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SessionKey(<redacted>)")
    }
}

impl SessionKey {
    /// Read the site's key, creating one if there is none.
    ///
    /// Stored as hex through the document store, which gives the atomic
    /// write and the owner-only mode for free. A key file that exists but
    /// does not parse is an error rather than a reason to mint a new one:
    /// replacing it would silently sign every existing session out and hide
    /// whatever damaged the file.
    pub fn load_or_create(store: &DocumentStore) -> Result<SessionKey, String> {
        let path = Path::new(SESSION_KEY_FILE);
        if let Some(document) = store.read(path).map_err(|e| e.to_string())? {
            return accounts::from_hex_32(&document.text)
                .map(SessionKey)
                .ok_or_else(|| {
                    format!(
                        "{} is not a 32-byte hex key. Delete it to start a new one, \
                         which signs every existing session out.",
                        store.root().join(SESSION_KEY_FILE).display()
                    )
                });
        }

        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes)
            .map_err(|e| format!("could not read system randomness: {e}"))?;
        let text = format!("{}\n", accounts::to_hex(&bytes));
        match store.write_private(path, &text, &Expectation::Absent) {
            Ok(_) => Ok(SessionKey(bytes)),
            // Another process created it between the read and the write.
            // Theirs is the key now; discard the one just generated rather
            // than overwriting and invalidating their sessions.
            Err(crate::project::store::StoreError::Conflict { .. }) => {
                let document = store
                    .read(path)
                    .map_err(|e| e.to_string())?
                    .ok_or_else(|| "the session key vanished as it was written".to_string())?;
                accounts::from_hex_32(&document.text)
                    .map(SessionKey)
                    .ok_or_else(|| "the session key was written malformed".to_string())
            }
            Err(e) => Err(e.to_string()),
        }
    }

    fn sign(&self, user: &UserId, credential_version: u64, expires: i64) -> blake3::Hash {
        let payload = format!(
            "{SESSION_DOMAIN}|{}|{credential_version}|{expires}",
            user.as_str()
        );
        blake3::keyed_hash(&self.0, payload.as_bytes())
    }

    /// A cookie value for `user`, valid until `expires`.
    pub fn mint(&self, user: &UserId, credential_version: u64, expires: i64) -> String {
        let mac = self.sign(user, credential_version, expires);
        format!(
            "{}.{credential_version}.{expires}.{}",
            user.as_str(),
            mac.to_hex()
        )
    }

    /// The user and credential version a cookie attests to, if its signature
    /// holds and it has not expired.
    ///
    /// Returns nothing rather than a reason: every way this can fail looks
    /// the same to the caller, who is either a browser with a stale cookie
    /// or someone guessing.
    pub fn verify(&self, cookie: &str, now: i64) -> Option<(UserId, u64)> {
        // The user part is a slug, which cannot contain '.', so a fixed
        // four-way split is unambiguous.
        let mut parts = cookie.splitn(4, '.');
        let user = UserId::new(parts.next()?).ok()?;
        let credential_version: u64 = parts.next()?.parse().ok()?;
        let expires: i64 = parts.next()?.parse().ok()?;
        let presented = blake3::Hash::from_hex(parts.next()?).ok()?;

        // Signature first, then expiry: an expired cookie is still evidence
        // of a real login, and checking in this order keeps both branches
        // doing the same constant-time comparison.
        let expected = self.sign(&user, credential_version, expires);
        if presented != expected || now >= expires {
            return None;
        }
        Some((user, credential_version))
    }
}

/// `Set-Cookie` for a fresh session.
pub fn session_cookie(value: &str, ttl_seconds: i64) -> String {
    format!("{SESSION_COOKIE}={value}; Path=/; HttpOnly; SameSite=Lax; Max-Age={ttl_seconds}")
}

/// `Set-Cookie` that clears the session.
pub fn cleared_cookie() -> String {
    format!("{SESSION_COOKIE}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0")
}

/// The value of one cookie from a request's `Cookie` header.
pub(super) fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(key, _)| *key == name)
        .map(|(_, value)| value.to_string())
}

/// Why a caller's effective role is lower than their account's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoleCap {
    /// `--read-only`.
    ReadOnlyServer,
    /// The catalog is not a project, so there is nowhere to write.
    NotAProject,
    /// The project is archived: read-only, interpretations still
    /// exportable (#214).
    Archived,
    /// The request came with an API token whose grant here is below the
    /// account's membership (#194).
    Token,
}

/// The API token a request came with (#194), as it applies in one project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenLimit {
    /// The token's name, for the audit log and for refusals.
    pub name: String,
    /// The grant's ceiling here.
    pub role: Role,
    pub download: DownloadScope,
    /// What the account itself has here, so a refusal can say whether the
    /// token or the account is the reason.
    pub account_role: Role,
    pub account_download: DownloadScope,
}

/// Who is asking, and what they may do.
///
/// Resolved once per request by [`middleware`] and read from the request
/// extensions by every handler that needs it, so a route cannot accidentally
/// resolve it twice and get two answers.
#[derive(Debug, Clone)]
pub struct Caller {
    /// `None` for an anonymous reader, who has no preferences and can write
    /// nothing.
    pub user: Option<UserId>,
    /// Whether this account administers the whole site (#214): it creates
    /// projects and accounts and acts as an administrator in every project.
    pub server_admin: bool,
    /// Whether the caller may see this project at all. False for a
    /// non-member of a project that requires a login; the middleware answers
    /// `404` rather than `403`, so the project's existence does not leak.
    /// Always true outside a site.
    pub visible: bool,
    /// What this caller may actually do here and now.
    pub role: Role,
    pub download: DownloadScope,
    /// Set when [`Self::role`] is capped below what the membership grants,
    /// so a refusal can explain which of the two reasons it was.
    pub cap: Option<RoleCap>,
    /// The API token the request came with, if it did (#194). [`Self::role`]
    /// and [`Self::download`] already include its ceiling.
    pub token: Option<TokenLimit>,
    /// Whether the site has any accounts at all. Decides whether an
    /// anonymous caller is offered a login or told there is nothing to log
    /// in to.
    pub authentication_configured: bool,
    /// The project's read policy, carried here rather than looked up again
    /// by the middleware.
    ///
    /// It is not a property of the caller, and sits on this struct anyway
    /// for two reasons: it comes out of the same membership read, so asking
    /// separately would parse the file twice on every request including
    /// every image chunk; and two reads could disagree with each other
    /// within one request, which is a strange way to decide whether that
    /// request is allowed.
    pub requires_login_to_read: bool,
}

impl Caller {
    pub fn is_authenticated(&self) -> bool {
        self.user.is_some()
    }

    /// What to show as the caller's name.
    pub fn display_name(&self) -> &str {
        self.user.as_ref().map_or("anonymous", |u| u.as_str())
    }

    /// Who did something, for an audit log: the account, and the token when
    /// a script acted for it (#194). Never used as an identity, which is
    /// what [`Self::display_name`] and [`Self::user`] are for.
    pub fn audit_name(&self) -> String {
        match &self.token {
            Some(token) => format!("{} via token {}", self.display_name(), token.name),
            None => self.display_name().to_string(),
        }
    }

    pub fn may(&self, needed: Role) -> bool {
        self.role >= needed
    }

    pub fn may_download(&self, needed: DownloadScope) -> bool {
        self.download >= needed
    }

    /// Refuse unless the caller is at least `needed`.
    ///
    /// The single place that decides between "log in and try again" and
    /// "this will never work", so no route has to get that right on its own:
    ///
    /// - no project at all -> `409`, because nothing about the request needs
    ///   fixing and no login would help;
    /// - `--read-only` -> `403`, naming the flag, because the operator of
    ///   the server is the one who can change it;
    /// - anonymous on a project with accounts -> `401`, which is the one
    ///   case where retrying after logging in works;
    /// - signed in but not permitted -> `403`, naming both roles.
    pub fn require(&self, needed: Role, action: &str) -> Result<(), ApiError> {
        if self.may(needed) {
            return Ok(());
        }
        match self.cap {
            Some(RoleCap::NotAProject) => Err(ApiError::conflict(
                "not_a_project",
                format!(
                    "This catalog is not a Ridal project, so there is nowhere to \
                     {action}. Run `ridal project init` in the directory you are \
                     serving, then restart."
                ),
            )),
            Some(RoleCap::ReadOnlyServer) => Err(ApiError::forbidden(
                "read_only",
                format!("This server was started read-only, so you cannot {action}."),
            )),
            Some(RoleCap::Archived) => Err(ApiError::forbidden(
                "archived",
                format!(
                    "This project is archived, so you cannot {action}. An archived \
                     project is read-only; its interpretations can still be exported."
                ),
            )),
            Some(RoleCap::Token)
                if self
                    .token
                    .as_ref()
                    .is_some_and(|token| token.account_role >= needed) =>
            {
                Err(token_limit(
                    self,
                    &format!("act as '{needed}', which {action} needs"),
                ))
            }
            None if !self.is_authenticated() => Err(ApiError::unauthorized(
                "authentication_required",
                format!("Sign in to {action}."),
            )),
            // A token below its account's role, where the account could not
            // do this either: the account is the reason worth naming.
            None | Some(RoleCap::Token) => Err(ApiError::forbidden(
                "insufficient_role",
                format!(
                    "You are '{}' ({}), and {action} needs the '{needed}' role or above.",
                    self.display_name(),
                    self.role
                ),
            )),
        }
    }

    /// Refuse unless the caller's download scope reaches `needed`.
    pub fn require_download(&self, needed: DownloadScope, what: &str) -> Result<(), ApiError> {
        if self.may_download(needed) {
            return Ok(());
        }
        if self
            .token
            .as_ref()
            .is_some_and(|token| token.account_download >= needed)
        {
            return Err(token_limit(self, &format!("download {what}")));
        }
        if !self.is_authenticated() && self.authentication_configured {
            return Err(ApiError::unauthorized(
                "authentication_required",
                format!("Sign in to download {what}."),
            ));
        }
        Err(ApiError::forbidden(
            "download_not_permitted",
            format!(
                "Downloading {what} needs the '{needed}' download scope, and \
                 '{}' has '{}'. An administrator sets this in Access settings.",
                self.display_name(),
                self.download
            ),
        ))
    }
}

/// A refusal whose reason is the token's ceiling, not the account (#194).
fn token_limit(caller: &Caller, what: &str) -> ApiError {
    let (name, role, download) = caller
        .token
        .as_ref()
        .map(|token| (token.name.as_str(), token.role, token.download))
        .unwrap_or(("?", caller.role, caller.download));
    ApiError::forbidden(
        "token_limit",
        format!(
            "The token '{name}' is limited to '{role}' with the '{download}' download \
             scope in this project, so it cannot {what}. Its account can; a token \
             with a higher ceiling here would too."
        ),
    )
}

/// The one person using `ridal gui`, or a project router built on its own
/// (as the route tests do).
///
/// There are no accounts to sign in to: the person at the machine is the
/// local [`DEFAULT_USER`](crate::identity::DEFAULT_USER) with everything an
/// unauthenticated Ridal has always allowed. Not `admin`, because what that
/// would add -- members and the access policy -- is about accounts a lone
/// project does not have. A bare directory of radargrams has nowhere to
/// write, so there everyone is a viewer.
pub fn local_caller(has_project: bool, read_only: bool) -> Caller {
    if !has_project {
        return Caller {
            user: None,
            server_admin: false,
            visible: true,
            role: Role::Viewer,
            download: DownloadScope::All,
            cap: Some(RoleCap::NotAProject),
            token: None,
            authentication_configured: false,
            requires_login_to_read: false,
        };
    }
    let (role, cap) = if read_only {
        (Role::Viewer, Some(RoleCap::ReadOnlyServer))
    } else {
        (Role::Operator, None)
    };
    Caller {
        user: UserId::new(crate::identity::DEFAULT_USER).ok(),
        server_admin: false,
        visible: true,
        role,
        download: DownloadScope::All,
        cap,
        token: None,
        authentication_configured: false,
        requires_login_to_read: false,
    }
}

/// Who a site account is, as the site resolved it from the session cookie
/// or an API token.
pub struct SiteIdentity<'a> {
    /// The signed-in account, or `None` for an anonymous caller.
    pub user: Option<&'a UserId>,
    pub server_admin: bool,
    /// Whether the site has an account file at all.
    pub accounts_configured: bool,
    /// The token's name and its grant in this project, when the request
    /// came with one (#194). The site has already refused a token with no
    /// grant here.
    pub token: Option<(&'a str, &'a crate::site::tokens::Grant)>,
}

/// What a site account may do in one project (#214).
///
/// A server administrator acts as an administrator in every project, with
/// nothing withheld. Anyone else gets their membership, or -- when the
/// project is readable without a login -- the anonymous viewer role and
/// download scope. A non-member of a project that requires a login is not
/// visible, which the middleware turns into a `404` rather than naming the
/// project and refusing it.
///
/// Reads the membership file per request: a small JSON file, and the cost
/// of a change taking effect at once rather than when a cookie ages out. A
/// file that will not parse fails closed ([`members::read_for_access`]).
pub fn project_caller(
    identity: &SiteIdentity<'_>,
    project: Option<&Project>,
    archived: bool,
    read_only: bool,
) -> Caller {
    let members = project
        .map(|project| members::read_for_access(project.documents()))
        .unwrap_or_default();
    let member = identity.user.and_then(|name| members.get(name));

    let (granted, download) = if identity.server_admin {
        (Role::Admin, DownloadScope::All)
    } else if let Some(member) = member {
        (member.role, member.download)
    } else {
        (Role::Viewer, members.anonymous_download)
    };
    let visible = identity.server_admin || member.is_some() || !members.require_auth_to_read;

    // A token never has more than its account, and never more than its
    // grant: the lower of the two, read per request.
    let token = identity.token.map(|(name, grant)| TokenLimit {
        name: name.to_string(),
        role: grant.role,
        download: grant.download,
        account_role: granted,
        account_download: download,
    });
    let (granted, download, token_cap) = match &token {
        Some(token) => (
            granted.min(token.role),
            download.min(token.download),
            (token.role < token.account_role).then_some(RoleCap::Token),
        ),
        None => (granted, download, None),
    };

    // The cap is recorded whenever it applies, not only when it actually
    // lowers the role, so a refusal names the reason rather than suggesting
    // a sign-in that cannot help.
    let (role, cap) = if read_only {
        (granted.min(Role::Viewer), Some(RoleCap::ReadOnlyServer))
    } else if archived {
        (granted.min(Role::Viewer), Some(RoleCap::Archived))
    } else {
        (granted, token_cap)
    };

    Caller {
        user: identity.user.cloned(),
        server_admin: identity.server_admin,
        visible,
        role,
        download,
        cap,
        token,
        requires_login_to_read: members.require_auth_to_read,
        authentication_configured: identity.accounts_configured,
    }
}

/// The account behind a request's session cookie, if the signature holds and
/// the account's credential version still matches the one it was minted at.
///
/// The version check is the revocation: a cookie minted before a password
/// change, a demotion or a deletion names a version the account no longer
/// has, or an account that is gone.
pub fn signed_in<'a>(
    accounts: &'a accounts::AccountSet,
    key: &SessionKey,
    headers: &HeaderMap,
    now: i64,
) -> Option<&'a accounts::Account> {
    let cookie = cookie_value(headers, SESSION_COOKIE)?;
    let (name, version) = key.verify(&cookie, now)?;
    let account = accounts.get(&name)?;
    (account.credential_version == version).then_some(account)
}

/// Paths reachable without a session even when the project requires one to
/// read: the health check, and the assets a page needs to render at all.
fn is_public_path(path: &str) -> bool {
    path == "/favicon.ico" || path == "/api/v1/health" || path.starts_with("/static/")
}

/// Hand every handler its [`Caller`].
///
/// A project served by a site receives the caller the site resolved
/// ([`project_caller`]); one that arrives without it fails closed, since the
/// site is the only thing that may decide who someone is. A project served
/// on its own is the local person ([`local_caller`]).
///
/// Also enforces the one policy that has to apply to whole pages rather than
/// to single routes: a project may require a login to read at all. Doing it
/// here rather than per route means a route added later cannot forget.
pub async fn middleware(
    State(state): State<Arc<AppState>>,
    mut request: Request,
    next: Next,
) -> Response {
    let path = request.uri().path().to_string();
    let caller = match (request.extensions().get::<Caller>(), &state.site) {
        (Some(caller), _) => caller.clone(),
        (None, None) => local_caller(state.project.is_some(), state.access.read_only),
        (None, Some(_)) => {
            return ApiError::internal(
                "caller_unresolved",
                "The request reached a project without an identity from its site. \
                 This is a bug in how the router was assembled.",
            )
            .into_response()
        }
    };

    // Someone not signed in, on a project that needs a login, is asked to
    // sign in. Asked before visibility is considered: they are not visible
    // either, but "sign in" is the answer that can help, and the site gives
    // the same answer for a key that does not exist, so it says nothing
    // about which projects do.
    if caller.requires_login_to_read
        && !caller.is_authenticated()
        && caller.authentication_configured
        && !is_public_path(&path)
    {
        return login_required(&path);
    }

    // A project the caller may not see answers 404, not 403, so its
    // existence does not leak (#214). A readable key is not a secret; access
    // comes from membership.
    if !caller.visible {
        return hidden_project(&path);
    }

    request.extensions_mut().insert(caller);
    next.run(request).await
}

/// The answer to someone not signed in who asks for a project that needs a
/// login: the login page for a page, a `401` for the API.
pub fn login_required(path: &str) -> Response {
    if path.starts_with("/api/") {
        ApiError::unauthorized(
            "authentication_required",
            "This project requires a login to read.",
        )
        .into_response()
    } else {
        // A person who followed a link wants the page, not a status code.
        // `next` is dropped rather than round-tripped: reflecting a URL out
        // of a request and back into a redirect is how open redirects
        // happen, and the site's projects are one click from the login page
        // anyway.
        Redirect::to("/login").into_response()
    }
}

/// The answer for a project the caller may not see: the same `404` as for
/// one that does not exist.
pub fn hidden_project(path: &str) -> Response {
    let error = ApiError::not_found(
        "project_not_found",
        "No such project, or you are not a member of it.",
    );
    if path.starts_with("/api/") {
        error.into_response()
    } else {
        super::routes::PageError(error).into_response()
    }
}

/// Seconds since the epoch.
pub fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

impl<S: Send + Sync> FromRequestParts<S> for Caller {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        parts.extensions.get::<Caller>().cloned().ok_or_else(|| {
            // Only reachable if a router is built without the middleware,
            // which is a programming error rather than a request fault --
            // and one that must fail closed rather than default to a
            // permissive caller.
            ApiError::internal(
                "caller_unresolved",
                "The request reached a handler without an identity. This is a bug \
                 in how the router was assembled.",
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;

    use super::*;
    use crate::project::store::DocumentStore;

    fn key() -> SessionKey {
        SessionKey([7u8; 32])
    }

    fn user(name: &str) -> UserId {
        UserId::new(name).unwrap()
    }

    #[test]
    fn a_minted_cookie_verifies_and_names_its_user() {
        let key = key();
        let now = 1_700_000_000;
        let cookie = key.mint(&user("erik"), 3, now + 100);

        let (name, version) = key.verify(&cookie, now).expect("must verify");
        assert_eq!(name.as_str(), "erik");
        assert_eq!(version, 3);
    }

    #[test]
    fn a_cookie_signed_with_another_key_is_refused() {
        // The whole point: the cookie is self-describing, so nothing but the
        // signature stops a client writing its own.
        let now = 1_700_000_000;
        let cookie = key().mint(&user("erik"), 1, now + 100);
        let other = SessionKey([9u8; 32]);
        assert!(other.verify(&cookie, now).is_none());
    }

    #[test]
    fn tampering_with_any_field_invalidates_the_cookie() {
        let key = key();
        let now = 1_700_000_000;
        let cookie = key.mint(&user("student"), 1, now + 100);
        let mac = cookie.rsplit_once('.').unwrap().1;

        for forged in [
            format!("erik.1.{}.{mac}", now + 100),
            format!("student.2.{}.{mac}", now + 100),
            format!("student.1.{}.{mac}", now + 10_000),
            format!("student.1.{}.{}", now + 100, "0".repeat(64)),
        ] {
            assert!(key.verify(&forged, now).is_none(), "{forged}");
        }
    }

    #[test]
    fn an_expired_cookie_is_refused() {
        let key = key();
        let expires = 1_700_000_000;
        let cookie = key.mint(&user("erik"), 1, expires);
        assert!(key.verify(&cookie, expires - 1).is_some());
        assert!(key.verify(&cookie, expires).is_none());
        assert!(key.verify(&cookie, expires + 1).is_none());
    }

    #[test]
    fn a_malformed_cookie_is_refused_rather_than_panicking() {
        let key = key();
        for junk in ["", ".", "erik", "erik.1", "erik.x.1.abc", "erik.1.1.nothex"] {
            assert!(key.verify(junk, 0).is_none(), "{junk}");
        }
    }

    #[test]
    fn the_session_key_persists_so_a_restart_does_not_sign_everyone_out() {
        // The reason a stateless cookie is the smaller option: surviving a
        // restart falls out of the key being persistent.
        let dir = tempfile::tempdir().unwrap();
        let store = DocumentStore::new(dir.path().to_path_buf());

        let first = SessionKey::load_or_create(&store).unwrap();
        let cookie = first.mint(&user("erik"), 1, 1_700_000_100);

        let second = SessionKey::load_or_create(&store).unwrap();
        assert!(
            second.verify(&cookie, 1_700_000_000).is_some(),
            "a reloaded key must still verify sessions it signed"
        );
    }

    #[test]
    #[cfg(unix)]
    fn the_session_key_is_written_readable_only_by_its_owner() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let store = DocumentStore::new(dir.path().to_path_buf());
        SessionKey::load_or_create(&store).unwrap();

        let mode = std::fs::metadata(dir.path().join(SESSION_KEY_FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "{mode:o}");
    }

    #[test]
    fn a_damaged_key_file_is_reported_rather_than_replaced() {
        // Replacing it would sign every session out and hide whatever
        // damaged it.
        let dir = tempfile::tempdir().unwrap();
        let store = DocumentStore::new(dir.path().to_path_buf());
        std::fs::write(dir.path().join(SESSION_KEY_FILE), "not a key").unwrap();

        let error = SessionKey::load_or_create(&store).unwrap_err();
        assert!(error.contains("32-byte hex"), "{error}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join(SESSION_KEY_FILE)).unwrap(),
            "not a key",
            "the damaged file was overwritten"
        );
    }

    #[test]
    fn a_cookie_is_found_among_others_and_never_invented() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            "theme=dark; ridal_session=abc.1.2.def; other=x"
                .parse()
                .unwrap(),
        );
        assert_eq!(
            cookie_value(&headers, SESSION_COOKIE).as_deref(),
            Some("abc.1.2.def")
        );
        assert_eq!(cookie_value(&headers, "nothing"), None);
        assert_eq!(cookie_value(&HeaderMap::new(), SESSION_COOKIE), None);
    }

    #[test]
    fn the_cleared_cookie_expires_immediately_and_carries_no_value() {
        let cleared = cleared_cookie();
        assert!(cleared.contains("Max-Age=0"), "{cleared}");
        assert!(cleared.starts_with("ridal_session=;"), "{cleared}");
        assert!(cleared.contains("HttpOnly"), "{cleared}");
    }

    #[test]
    fn a_session_cookie_is_http_only_and_same_site_but_not_secure() {
        // Not `Secure`: Ridal essentially never sees HTTPS, so the attribute
        // would stop the cookie being sent over the loopback HTTP that both
        // `ridal gui` and every reverse-proxy deployment speak.
        let cookie = session_cookie("erik.1.2.abc", 60);
        assert!(cookie.contains("HttpOnly"), "{cookie}");
        assert!(cookie.contains("SameSite=Lax"), "{cookie}");
        assert!(!cookie.contains("Secure"), "{cookie}");
        assert!(cookie.contains("Max-Age=60"), "{cookie}");
    }

    #[test]
    fn a_pages_assets_stay_reachable_without_a_session() {
        for path in ["/static/app.css", "/static/login.js", "/favicon.ico"] {
            assert!(is_public_path(path), "{path}");
        }
        for path in ["/", "/view/line-01", "/api/v1/datasets", "/settings"] {
            assert!(!is_public_path(path), "{path}");
        }
    }

    #[test]
    fn the_local_person_operates_a_project_and_views_a_bare_directory() {
        let local = local_caller(true, false);
        assert_eq!(local.display_name(), crate::identity::DEFAULT_USER);
        assert_eq!(local.role, Role::Operator);
        assert!(!local.authentication_configured);

        let read_only = local_caller(true, true);
        assert_eq!(read_only.role, Role::Viewer);
        assert_eq!(read_only.cap, Some(RoleCap::ReadOnlyServer));

        let bare = local_caller(false, false);
        assert!(bare.user.is_none());
        assert_eq!(bare.cap, Some(RoleCap::NotAProject));
    }

    #[test]
    fn a_site_caller_is_their_membership_capped_by_the_server_and_the_project() {
        let dir = tempfile::tempdir().unwrap();
        let project = Project::init(dir.path(), None).unwrap();
        members::update(project.documents(), |set| {
            set.require_auth_to_read = true;
            set.upsert(&user("anna"), Role::Picker, DownloadScope::Picks);
            Ok(())
        })
        .unwrap();
        fn identity(name: &UserId, server_admin: bool) -> SiteIdentity<'_> {
            SiteIdentity {
                user: Some(name),
                server_admin,
                accounts_configured: true,
                token: None,
            }
        }
        let (anna, bo, cy) = (&user("anna"), &user("bo"), &user("cy"));

        let member = project_caller(&identity(anna, false), Some(&project), false, false);
        assert!(member.visible);
        assert_eq!(
            (member.role, member.download),
            (Role::Picker, DownloadScope::Picks)
        );

        // Not a member of a project that requires a login: invisible.
        let stranger = project_caller(&identity(bo, false), Some(&project), false, false);
        assert!(!stranger.visible);

        // A server administrator is an administrator everywhere...
        let admin = project_caller(&identity(cy, true), Some(&project), false, false);
        assert!(admin.visible);
        assert_eq!(admin.role, Role::Admin);
        // ...until the project is archived or the server read-only, and the
        // refusal says which.
        let archived = project_caller(&identity(cy, true), Some(&project), true, false);
        assert_eq!(
            (archived.role, archived.cap),
            (Role::Viewer, Some(RoleCap::Archived))
        );
        let read_only = project_caller(&identity(cy, true), Some(&project), true, true);
        assert_eq!(read_only.cap, Some(RoleCap::ReadOnlyServer));
    }

    #[test]
    fn a_refusal_says_which_of_the_reasons_it_was() {
        let base = Caller {
            user: None,
            server_admin: false,
            visible: true,
            role: Role::Viewer,
            download: DownloadScope::All,
            cap: None,
            token: None,
            authentication_configured: true,
            requires_login_to_read: false,
        };

        // Anonymous, on a project with accounts: retrying after a login
        // works, which is the one case that is a 401.
        let error = base.require(Role::Picker, "save picks").unwrap_err();
        assert_eq!(error.status_code(), StatusCode::UNAUTHORIZED);

        let signed_in = Caller {
            user: Some(user("student")),
            role: Role::Picker,
            ..base.clone()
        };
        assert!(signed_in.require(Role::Picker, "save picks").is_ok());
        let error = signed_in
            .require(Role::Operator, "edit layers")
            .unwrap_err();
        assert_eq!(error.status_code(), StatusCode::FORBIDDEN);
        assert!(error.message().contains("operator"), "{}", error.message());

        let read_only = Caller {
            user: Some(user("erik")),
            role: Role::Viewer,
            cap: Some(RoleCap::ReadOnlyServer),
            ..base.clone()
        };
        let error = read_only.require(Role::Picker, "save picks").unwrap_err();
        assert_eq!(error.status_code(), StatusCode::FORBIDDEN);
        assert!(error.message().contains("read-only"), "{}", error.message());

        let bare = Caller {
            cap: Some(RoleCap::NotAProject),
            authentication_configured: false,
            ..base
        };
        let error = bare.require(Role::Picker, "save picks").unwrap_err();
        assert_eq!(error.status_code(), StatusCode::CONFLICT);
        assert!(
            error.message().contains("project init"),
            "{}",
            error.message()
        );
    }

    #[test]
    fn download_scope_refusals_distinguish_anonymous_from_restricted() {
        let anonymous = Caller {
            user: None,
            server_admin: false,
            visible: true,
            role: Role::Viewer,
            download: DownloadScope::None,
            cap: None,
            token: None,
            authentication_configured: true,
            requires_login_to_read: false,
        };
        let error = anonymous
            .require_download(DownloadScope::Derived, "level 2 points")
            .unwrap_err();
        assert_eq!(error.status_code(), StatusCode::UNAUTHORIZED);

        let restricted = Caller {
            user: Some(user("student")),
            download: DownloadScope::Picks,
            ..anonymous
        };
        assert!(restricted
            .require_download(DownloadScope::Picks, "picks")
            .is_ok());
        let error = restricted
            .require_download(DownloadScope::All, "the radargram")
            .unwrap_err();
        assert_eq!(error.status_code(), StatusCode::FORBIDDEN);
        assert!(
            error.message().contains("Access settings"),
            "{}",
            error.message()
        );
    }

    #[test]
    fn a_token_refusal_names_the_token_only_when_it_is_the_reason() {
        let token = TokenLimit {
            name: "ci".to_string(),
            role: Role::Viewer,
            download: DownloadScope::Results,
            account_role: Role::Picker,
            account_download: DownloadScope::Picks,
        };
        let caller = Caller {
            user: Some(user("anna")),
            server_admin: false,
            visible: true,
            role: Role::Viewer,
            download: DownloadScope::Results,
            cap: Some(RoleCap::Token),
            token: Some(token),
            authentication_configured: true,
            requires_login_to_read: false,
        };
        assert_eq!(caller.audit_name(), "anna via token ci");

        // The account could: the token is the reason.
        let error = caller.require(Role::Picker, "save picks").unwrap_err();
        assert!(error.message().contains("'ci'"), "{}", error.message());
        let error = caller
            .require_download(DownloadScope::Picks, "picks")
            .unwrap_err();
        assert!(error.message().contains("'ci'"), "{}", error.message());

        // The account could not either: the account is the reason.
        let error = caller.require(Role::Operator, "edit layers").unwrap_err();
        assert!(!error.message().contains("'ci'"), "{}", error.message());
        let error = caller
            .require_download(DownloadScope::All, "the radargram")
            .unwrap_err();
        assert!(!error.message().contains("'ci'"), "{}", error.message());
    }
}
