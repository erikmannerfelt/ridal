//! `ridal gui` and `ridal server start` launch modes (#120). Both serve
//! through [`site::build_site_router`] (#214): `ridal server start` a whole
//! [`Site`] of projects, `ridal gui` a site of one, its project at
//! `/p/default/`. Only what they serve, bind behaviour, port selection and
//! browser-opening differ between them.

use std::net::{IpAddr, SocketAddr};
use std::path::Path;

use super::app::{AccessOptions, AppState};
use crate::server::render_service::RenderServiceConfig;
use crate::server::site::{self, SiteState};
use crate::site::Site;

/// `ridal gui`'s project: the one found at or above `root`, or `root` itself
/// as a bare, read-only directory of radargrams.
async fn serve_gui(
    root: &Path,
    read_only: bool,
    open_browser: bool,
    config: RenderServiceConfig,
) -> Result<(), String> {
    // A project is found by searching upwards, so pointing Ridal at a
    // subdirectory or at a single file inside a project still saves
    // interpretations to the right place.
    let project = crate::project::Project::discover(root).map_err(|e| e.to_string())?;
    if let Some(project) = project.as_ref() {
        warn_about_project_accounts(project);
        if !read_only {
            sweep_upload_temps(project);
        }
    }

    let state = AppState::build_with_project(
        root,
        &config,
        project,
        AccessOptions {
            read_only,
            // Nobody signs in to `ridal gui`.
            allow_password_login: false,
        },
    )?;
    let catalog = state.catalog();
    for warning in &catalog.warnings {
        eprintln!("Warning: {}", warning.message);
    }
    println!(
        "Discovered {} radargram(s) under {}",
        catalog.entries.len(),
        root.display()
    );
    match (state.project.as_ref(), read_only) {
        (Some(project), false) => println!(
            "Project {} (writable, as '{}')",
            project.root().display(),
            crate::identity::DEFAULT_USER
        ),
        (Some(project), true) => println!("Project {} (read-only)", project.root().display()),
        (None, _) => println!("No project here; interpretations cannot be saved."),
    }
    // Every root Ridal will not write to, whatever made it read-only: a
    // directory outside the project, or `--read-only` making all of them so.
    for root in state.roots.iter().filter(|root| !root.writable) {
        println!(
            "  {} (read-only; Ridal never writes outside the project)",
            root.path.display()
        );
    }

    let router = site::build_site_router(SiteState::local(state));
    let home = format!("/p/{}/", super::app::DEFAULT_PROJECT_KEY);
    serve(router, IpAddr::from([127, 0, 0, 1]), 0, &home, open_browser).await
}

/// Remove upload temporaries a crash left in a project, saying what went
/// (#302). A tidy start; a failure is a warning rather than refusal to
/// serve, since the catalog already skips these files.
fn sweep_upload_temps(project: &crate::project::Project) {
    match project.sweep_upload_temps() {
        Ok(0) => {}
        Ok(n) => eprintln!(
            "Removed {n} stale upload temporary file(s) from {}",
            project.root().display()
        ),
        Err(e) => eprintln!(
            "Warning: could not sweep stale uploads from {}: {e}",
            project.root().display()
        ),
    }
}

/// Say so when a project still holds accounts from before sites (#214).
///
/// `ridal gui` has no accounts, so they are simply not used: its one person
/// is the local user whatever the file says. Worth a line, because someone
/// who remembers signing in to this project will otherwise wonder where the
/// sign-in went. A site refuses such a project outright
/// ([`Site::project`]), since there the accounts would matter.
fn warn_about_project_accounts(project: &crate::project::Project) {
    let Ok(Some(document)) = project
        .documents()
        .read(Path::new(crate::project::members::MEMBERS_FILE))
    else {
        return;
    };
    if crate::site::looks_like_accounts(&document.text) {
        eprintln!(
            "Warning: {} holds accounts from before sites. `ridal gui` ignores them; \
             to serve this project with sign-ins, add it to a site (`ridal site init`).",
            project
                .documents()
                .root()
                .join(crate::project::members::MEMBERS_FILE)
                .display()
        );
    }
}

/// Serve a whole site (#214).
///
/// `root` must be a site, not a project: a project has no site-wide accounts,
/// and serving one through the site entry point would silently expose
/// whatever the project happened to hold. The refusal points at `ridal gui`
/// for local single-project work and at `ridal site init` for making a site.
async fn serve_site(
    root: &Path,
    host: IpAddr,
    port: u16,
    open_browser: bool,
    read_only: bool,
    allow_insecure_login: bool,
    config: RenderServiceConfig,
) -> Result<(), String> {
    // A project, found by walking upwards, is the one input that must be
    // refused rather than treated as a site. Checked before `Site::discover`,
    // because a project *inside* a site's `projects/` would otherwise resolve
    // to the enclosing site and serve every project when one was asked for.
    if crate::project::Project::discover(root)
        .map_err(|e| e.to_string())?
        .is_some()
    {
        return Err(format!(
            "{} is a project, not a site. Run `ridal site init` here, or use \
             `ridal gui` for local single-project work.",
            root.display()
        ));
    }
    let site = Site::discover(root)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| {
            format!(
                "{} is not a Ridal site. Run `ridal site init` here, or use \
                 `ridal gui` for local single-project work.",
                root.display()
            )
        })?;

    // Parsed here, not merely stat-ed: an account file that will not parse is
    // refused at startup, where an operator is watching, rather than turning
    // every later request into a silent denial.
    let accounts = crate::site::accounts::read(site.store())
        .map_err(|e| {
            format!(
                "Refusing to serve {}: its accounts cannot be read. {e}",
                site.root().display()
            )
        })?
        .is_some();
    if !accounts && !read_only {
        return Err(format!(
            "Refusing to serve {}: it has no accounts, so nobody could sign in to \
             manage or change anything. Create the first one with `ridal site \
             account add <name> --server-admin`, or start with --read-only to \
             publish its projects.",
            site.root().display()
        ));
    }
    check_bind_safety(host, accounts, allow_insecure_login)?;

    let site_name = site.name();
    let site_root = site.root().to_path_buf();
    let projects = site.list().map_err(|e| e.to_string())?;
    let project_count = projects.len();
    if !read_only {
        for key in &projects {
            let Ok(project) = site.project(key) else {
                continue;
            };
            sweep_upload_temps(&project);
        }
    }
    let state = SiteState::new(
        site,
        AccessOptions {
            read_only,
            allow_password_login: host.is_loopback() || allow_insecure_login,
        },
        config,
    );

    println!("Site {site_name} ({})", site_root.display());
    println!("  {project_count} project(s)");
    if accounts {
        println!("  authenticated");
    } else {
        println!("  read-only, no accounts");
    }

    serve(
        site::build_site_router(state),
        host,
        port,
        "/",
        open_browser,
    )
    .await
}

/// Bind, print where, optionally open a browser there, and serve until
/// Ctrl-C.
async fn serve(
    router: axum::Router,
    host: IpAddr,
    port: u16,
    home: &str,
    open_browser: bool,
) -> Result<(), String> {
    let addr = SocketAddr::new(host, port);
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| format!("Failed to bind {addr}: {e}"))?;
    let bound_addr = listener
        .local_addr()
        .map_err(|e| format!("Failed to read bound address: {e}"))?;
    let url = format!("http://{bound_addr}{home}");
    println!("Serving on {url}");

    if open_browser {
        if let Err(e) = webbrowser::open(&url) {
            eprintln!("Warning: could not open a browser automatically: {e}");
        }
    }

    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(|e| format!("Server error: {e}"))
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    println!("Shutting down.");
}

/// Cap glibc at [`MALLOC_ARENAS`] arenas for this process (#306). Called
/// first thing by both server entry points, before the runtime starts any
/// thread.
///
/// glibc gives each thread that allocates its own arena, up to eight per
/// core, and returns little of an arena's freed memory to the operating
/// system. Rendering runs on tokio's blocking pool, which grows to however
/// many renders are in flight, so a burst of cold overview builds left a
/// server permanently holding ~1 GB of freed memory across ~60 arenas:
/// measured on 120 radargrams requested at once, it ended at 0.8-1.9 GB
/// with no file open. With four arenas it peaked at 0.34 GB and ended at
/// 33 MB, for a cold pass about 10% slower. Nothing else here is
/// allocation-bound enough to feel the contention.
///
/// `MALLOC_ARENA_MAX` in the environment still wins, for anyone tuning a
/// deployment. A no-op on other allocators, which manage this themselves.
fn limit_malloc_arenas() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    if std::env::var_os("MALLOC_ARENA_MAX").is_none() {
        // SAFETY: a plain integer setting, made before any other thread
        // exists.
        unsafe {
            libc::mallopt(libc::M_ARENA_MAX, MALLOC_ARENAS);
        }
    }
}

/// glibc arenas a server may use; see [`limit_malloc_arenas`].
#[cfg(all(target_os = "linux", target_env = "gnu"))]
const MALLOC_ARENAS: libc::c_int = 4;

/// `ridal gui`: local convenience mode. Binds loopback only and selects an
/// available port. The URL is always printed; a browser is opened only when
/// `--open-browser` was passed, and a failure to open it is a warning, never
/// a reason to stop the server (#120, #200).
pub fn run_gui(
    root: &Path,
    read_only: bool,
    open_browser: bool,
    config: RenderServiceConfig,
) -> Result<(), String> {
    limit_malloc_arenas();
    let runtime = tokio::runtime::Runtime::new()
        .map_err(|e| format!("Failed to start async runtime: {e}"))?;
    runtime.block_on(serve_gui(root, read_only, open_browser, config))
}

/// `ridal server start`: deployment-oriented mode. Loopback by default;
/// remote binding is explicit, and what a remote bind may serve is decided
/// by [`check_bind_safety`].
#[allow(clippy::too_many_arguments)]
pub fn run_server_start(
    root: &Path,
    host: IpAddr,
    port: u16,
    open_browser: bool,
    read_only: bool,
    allow_insecure_login: bool,
    config: RenderServiceConfig,
) -> Result<(), String> {
    limit_malloc_arenas();
    let runtime = tokio::runtime::Runtime::new()
        .map_err(|e| format!("Failed to start async runtime: {e}"))?;
    runtime.block_on(serve_site(
        root,
        host,
        port,
        open_browser,
        read_only,
        allow_insecure_login,
        config,
    ))
}

/// Whether a bind address may carry the site's password sign-ins.
///
/// Keyed on the bind address rather than on the connection, because
/// **Ridal will essentially never see HTTPS**: behind a TLS-terminating
/// proxy it sees plain HTTP on loopback, which is correct and safe, so "is
/// this connection TLS?" always answers no and is useless as a guardrail. A
/// loopback bind means either it is genuinely local, or there is a proxy in
/// front.
///
/// On any other bind a sign-in puts a password on the wire in the clear.
/// That needs a flag rather than a refusal, because the operator may
/// legitimately have a TLS proxy Ridal cannot see: `--allow-insecure-login`
/// reads as "I have put this on the network and I accept what is in front
/// of it". A site with no accounts has no passwords to protect; it is
/// served read-only or not at all ([`serve_site`]).
fn check_bind_safety(
    host: IpAddr,
    accounts: bool,
    allow_insecure_login: bool,
) -> Result<(), String> {
    if host.is_loopback() || !accounts || allow_insecure_login {
        return Ok(());
    }
    Err(format!(
        "Refusing to accept password logins on {host}: Ridal does not terminate \
         TLS, so a password sent to this address travels in the clear unless \
         something in front of it is doing so. Bind loopback behind a \
         TLS-terminating reverse proxy -- the supported way to serve this \
         remotely -- or re-run with --allow-insecure-login to accept what is \
         in front of this address."
    ))
}

#[cfg(test)]
mod tests {
    use super::check_bind_safety;
    use std::net::IpAddr;

    fn ip(text: &str) -> IpAddr {
        text.parse().unwrap()
    }

    #[test]
    fn loopback_may_carry_passwords() {
        // Either it is genuinely local, or there is a TLS proxy in front.
        for accounts in [false, true] {
            assert!(check_bind_safety(ip("127.0.0.1"), accounts, false).is_ok());
            assert!(check_bind_safety(ip("::1"), accounts, false).is_ok());
        }
    }

    #[test]
    fn a_remote_bind_with_accounts_refuses_cleartext_passwords_unless_told_to() {
        let error = check_bind_safety(ip("0.0.0.0"), true, false).unwrap_err();
        assert!(error.contains("--allow-insecure-login"), "{error}");
        assert!(error.contains("reverse proxy"), "{error}");
        assert!(check_bind_safety(ip("192.168.1.10"), true, false).is_err());

        // The flag reads as "I accept what is in front of this address",
        // which is the honest shape: Ridal cannot see the TLS proxy that
        // makes this fine.
        assert!(check_bind_safety(ip("0.0.0.0"), true, true).is_ok());
    }

    /// `ridal server start` with everything but the path and the flags at
    /// their defaults. Every case below is refused before anything binds.
    async fn start(root: &std::path::Path, host: &str, read_only: bool) -> String {
        super::serve_site(
            root,
            ip(host),
            0,
            false,
            read_only,
            false,
            crate::server::render_service::RenderServiceConfig::default(),
        )
        .await
        .unwrap_err()
    }

    #[tokio::test]
    async fn server_start_refuses_what_is_not_a_servable_site() {
        // A project, including one inside a site's `projects/`, is not a site.
        let project = tempfile::tempdir().unwrap();
        crate::project::Project::init(project.path(), None).unwrap();
        let refused = start(project.path(), "127.0.0.1", false).await;
        assert!(refused.contains("is a project, not a site"), "{refused}");

        let bare = tempfile::tempdir().unwrap();
        let refused = start(bare.path(), "127.0.0.1", false).await;
        assert!(refused.contains("not a Ridal site"), "{refused}");

        // A site nobody can sign in to is served read-only or not at all.
        let site = tempfile::tempdir().unwrap();
        let opened = crate::site::Site::init(site.path(), None).unwrap();
        let refused = start(site.path(), "127.0.0.1", false).await;
        assert!(refused.contains("no accounts"), "{refused}");

        // And with accounts, a network bind still guards the passwords.
        crate::site::accounts::update(opened.store(), |set| {
            set.users.push(crate::site::accounts::Account::new(
                crate::identity::UserId::new("anna").unwrap(),
                true,
            ));
            Ok(())
        })
        .unwrap();
        let refused = start(site.path(), "0.0.0.0", true).await;
        assert!(refused.contains("--allow-insecure-login"), "{refused}");
    }

    #[test]
    fn ridal_gui_names_accounts_it_will_ignore_and_is_quiet_otherwise() {
        // Only a warning, so there is nothing to assert but that each shape
        // of file is looked at without failing.
        let dir = tempfile::tempdir().unwrap();
        let project = crate::project::Project::init(dir.path(), None).unwrap();
        super::warn_about_project_accounts(&project);
        project
            .documents()
            .write(
                std::path::Path::new(crate::project::members::MEMBERS_FILE),
                r#"{"users": []}"#,
                &crate::project::store::Expectation::Any,
            )
            .unwrap();
        super::warn_about_project_accounts(&project);
    }

    #[test]
    fn a_site_with_no_accounts_has_no_password_to_guard() {
        // The read-only public arrangement: nothing to log in to.
        assert!(check_bind_safety(ip("0.0.0.0"), false, false).is_ok());
    }
}
