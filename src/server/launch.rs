//! `ridal gui` and `ridal server start` launch modes (#120). Both use the
//! same [`crate::server::app::build_router`] application; only bind
//! behavior, port selection, and browser-opening differ between them.

use std::net::{IpAddr, SocketAddr};
use std::path::Path;
use std::sync::Arc;

use super::app::AppState;
use super::render::service::RenderServiceConfig;

pub struct LaunchOptions {
    pub host: IpAddr,
    /// `0` requests an OS-assigned ephemeral port.
    pub port: u16,
    pub open_browser: bool,
}

async fn serve(
    root: &Path,
    options: LaunchOptions,
    config: RenderServiceConfig,
) -> Result<(), String> {
    let state = Arc::new(AppState::build(root, &config)?);
    if !state.catalog.warnings.is_empty() {
        for w in &state.catalog.warnings {
            eprintln!("Warning: {}", w.message);
        }
    }
    println!(
        "Discovered {} radargram(s) under {}",
        state.catalog.entries.len(),
        root.display()
    );

    let router = super::app::build_router(state);
    let addr = SocketAddr::new(options.host, options.port);
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| format!("Failed to bind {addr}: {e}"))?;
    let bound_addr = listener
        .local_addr()
        .map_err(|e| format!("Failed to read bound address: {e}"))?;
    let url = format!("http://{bound_addr}");
    println!("Serving on {url}");

    if options.open_browser {
        println!("{url}");
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

/// `ridal gui`: local convenience mode. Binds loopback only, selects an
/// available port, and opens a browser -- a failure to open the browser
/// is a warning, never a reason to stop the server (#120).
pub fn run_gui(root: &Path, config: RenderServiceConfig) -> Result<(), String> {
    let runtime = tokio::runtime::Runtime::new()
        .map_err(|e| format!("Failed to start async runtime: {e}"))?;
    runtime.block_on(serve(
        root,
        LaunchOptions {
            host: IpAddr::from([127, 0, 0, 1]),
            port: 0,
            open_browser: true,
        },
        config,
    ))
}

/// `ridal server start`: deployment-oriented mode. Loopback by default;
/// remote binding is explicit, and this milestone deliberately implements
/// no authentication -- documented as future work (#120), not silently
/// assumed safe.
pub fn run_server_start(
    root: &Path,
    host: IpAddr,
    port: u16,
    open_browser: bool,
    config: RenderServiceConfig,
) -> Result<(), String> {
    let runtime = tokio::runtime::Runtime::new()
        .map_err(|e| format!("Failed to start async runtime: {e}"))?;
    runtime.block_on(serve(
        root,
        LaunchOptions {
            host,
            port,
            open_browser,
        },
        config,
    ))
}
