//! Every operation in the OpenAPI description needs what its `x-ridal-needs`
//! says (#330), no more and no less.
//!
//! The *Needs* column of the HTTP reference is rendered from that
//! declaration, and the handler checks something else entirely
//! (`Caller::require`, or nothing at all). This ties the two: each operation
//! is sent the same request twice, by a caller just below what it declares,
//! which must be refused, and by one exactly at it, which must succeed. An
//! operation stricter than declared fails the second; a laxer one, the
//! first. A request that succeeds at the declared level is also one whose
//! refusal below it is about the caller, not about the request.
//!
//! Each operation needs a [`Case`]: what to put in its path and body. One
//! missing fails the test, so a route cannot be added to the spec without
//! one.

use std::path::Path as StdPath;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;

use super::access_tests::{
    activated, id, new_site, password, password_hash, project_key, site_router, write_people,
    UserSet, PROJECT,
};
use super::app::AccessOptions;
use super::auth::{self, SessionKey, SESSION_COOKIE};
use super::needs::{Needs, Who};
use crate::project::roles::{DownloadScope, Role};
use crate::site::accounts::{self, invite};
use crate::site::Site;

/// The account that holds a pending invite, for the operation that
/// redeems one.
const INVITEE: &str = "invitee";

/// What a [`Case`] can build its request from.
struct Fixture {
    /// The token of [`INVITEE`]'s invite.
    invite_token: String,
}

/// What to send to one operation, by `operationId`.
struct Case {
    operation: &'static str,
    /// The JSON body, if the operation takes one.
    body: fn(&Fixture) -> Option<Value>,
}

fn no_body(_: &Fixture) -> Option<Value> {
    None
}

/// One per operation in the spec.
const CASES: &[Case] = &[
    Case {
        operation: "health",
        body: no_body,
    },
    Case {
        operation: "openapi_json",
        body: no_body,
    },
    Case {
        operation: "me",
        body: no_body,
    },
    Case {
        operation: "login",
        body: |_| Some(json!({"name": "viewer", "password": password()})),
    },
    Case {
        operation: "logout",
        body: no_body,
    },
    Case {
        operation: "redeem_invite",
        body: |fixture| {
            Some(json!({"token": fixture.invite_token, "password": "a long new passphrase"}))
        },
    },
    Case {
        operation: "list_profiles",
        body: no_body,
    },
    Case {
        operation: "get_preferences",
        body: no_body,
    },
    Case {
        operation: "put_preferences",
        body: |_| Some(json!({"theme": "dark"})),
    },
    Case {
        operation: "get_settings",
        body: no_body,
    },
    Case {
        operation: "put_settings",
        body: |_| Some(json!({"default_profile": null})),
    },
];

/// Who sends a probe.
#[derive(Debug, Clone, Copy)]
enum Caller {
    Anonymous,
    /// A member of the project with this role, signed in.
    Member(Role),
}

/// One request to send, and what must come back.
#[derive(Debug)]
struct Probe {
    caller: Caller,
    /// Whether the project requires a login to read.
    private: bool,
    /// `None` for success, or the refusal's status.
    refused: Option<StatusCode>,
}

/// The two probes a declaration implies: one just below it and one at it.
fn probes(needs: &Needs) -> Vec<Probe> {
    let at = |caller, private| Probe {
        caller,
        private,
        refused: None,
    };
    let below = |caller, private, status| Probe {
        caller,
        private,
        refused: Some(status),
    };
    match needs.who {
        // Even in a project that requires a login, nobody is refused.
        Who::Nobody => vec![at(Caller::Anonymous, true)],
        Who::Anyone => vec![
            below(Caller::Anonymous, true, StatusCode::UNAUTHORIZED),
            at(Caller::Member(Role::Viewer), true),
        ],
        // A public project, so that being able to read it is not the reason.
        Who::SignedIn => vec![
            below(Caller::Anonymous, false, StatusCode::UNAUTHORIZED),
            at(Caller::Member(Role::Viewer), false),
        ],
        Who::Role(role) => {
            let lower = Role::ALL
                .into_iter()
                .take_while(|r| *r < role)
                .last()
                .expect("an endpoint that needs `viewer` would say `anyone`");
            vec![
                below(Caller::Member(lower), false, StatusCode::FORBIDDEN),
                at(Caller::Member(role), false),
            ]
        }
    }
}

/// A site with one member per role, the [`INVITEE`] with a pending invite,
/// and the project's read policy.
fn site(private: bool) -> (tempfile::TempDir, Fixture) {
    let dir = new_site();
    let hash = password_hash();
    let users = vec![
        activated("viewer", Role::Viewer, DownloadScope::None, hash),
        activated("picker", Role::Picker, DownloadScope::None, hash),
        activated("operator", Role::Operator, DownloadScope::None, hash),
        activated("admin", Role::Admin, DownloadScope::All, hash),
    ];
    write_people(
        dir.path(),
        &UserSet {
            require_auth_to_read: private,
            anonymous_download: DownloadScope::None,
            users,
        },
    );
    let (invite_token, pending) = invite::mint_for_project(
        auth::now(),
        project_key(),
        Role::Viewer,
        DownloadScope::None,
    )
    .unwrap();
    let site = Site::open(dir.path()).unwrap();
    accounts::update(site.store(), |set| {
        let mut account = accounts::Account::new(id(INVITEE), false);
        account.invite = Some(pending.clone());
        set.users.push(account);
        Ok(())
    })
    .unwrap();
    (dir, Fixture { invite_token })
}

/// A session cookie for `role`'s member, minted rather than signed in for,
/// so the probes do not pay for a password hash each.
fn session(root: &StdPath, role: Role) -> String {
    let key = SessionKey::load_or_create(Site::open(root).unwrap().store()).unwrap();
    let value = key.mint(&id(role.as_str()), 1, auth::now() + 3600);
    format!("{SESSION_COOKIE}={value}")
}

/// Every operation in the spec: `(operationId, method, path, needs)`.
fn operations() -> Vec<(String, String, String, Needs)> {
    let spec = serde_json::to_value(super::openapi::spec()).unwrap();
    let mut operations = Vec::new();
    for (path, item) in spec["paths"].as_object().unwrap() {
        for (method, operation) in item.as_object().unwrap() {
            if method == "parameters" {
                continue;
            }
            let needs: Needs =
                serde_json::from_value(operation[super::needs::EXTENSION].clone()).unwrap();
            operations.push((
                operation["operationId"].as_str().unwrap().to_string(),
                method.to_uppercase(),
                path.clone(),
                needs,
            ));
        }
    }
    operations
}

#[tokio::test]
#[serial_test::serial(netcdf)]
async fn every_operation_needs_what_it_declares() {
    let mut faults = Vec::new();
    for (operation, method, path, needs) in operations() {
        let Some(case) = CASES.iter().find(|case| case.operation == operation) else {
            faults.push(format!("{operation}: no Case in needs_tests::CASES"));
            continue;
        };
        let uri = path.replace("{key}", PROJECT);
        for probe in probes(&needs) {
            let (dir, fixture) = site(probe.private);
            let app = site_router(dir.path(), AccessOptions::default());
            let mut request = Request::builder().method(method.as_str()).uri(&uri);
            if let Caller::Member(role) = probe.caller {
                request = request.header(header::COOKIE, session(dir.path(), role));
            }
            let request = match (case.body)(&fixture) {
                Some(body) => request
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body.to_string())),
                None => request.body(Body::empty()),
            }
            .unwrap();
            let response = app.oneshot(request).await.unwrap();
            let status = response.status();
            let ok = match probe.refused {
                None => status.is_success(),
                Some(refused) => status == refused,
            };
            if !ok {
                let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
                    .await
                    .unwrap_or_default();
                faults.push(format!(
                    "{method} {path} ({operation}) declares {needs:?}; as {:?} in a {} \
                     project it answered {status}, expected {}: {}",
                    probe.caller,
                    if probe.private { "private" } else { "public" },
                    probe
                        .refused
                        .map_or("success".to_string(), |s| s.to_string()),
                    String::from_utf8_lossy(&bytes),
                ));
            }
        }
    }
    assert!(faults.is_empty(), "{faults:#?}");
}

/// A [`Case`] for an operation the spec no longer has is stale.
#[test]
fn every_case_names_an_operation() {
    let operations: Vec<String> = operations().into_iter().map(|(id, ..)| id).collect();
    let stale: Vec<&str> = CASES
        .iter()
        .map(|case| case.operation)
        .filter(|name| !operations.iter().any(|id| id == name))
        .collect();
    assert!(stale.is_empty(), "cases for no operation: {stale:?}");
}
