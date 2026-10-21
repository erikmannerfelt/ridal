//! What a running server writes to stderr for whoever runs it (#282).
//!
//! Three things, all through `tracing`:
//!
//! - **Events** the code already raises, such as a pick left out of an
//!   export or a processing job that failed. Without a subscriber they were
//!   all dropped, so [`install`] is the first job of both server commands.
//! - **An access log**: one line per request with its method, its route
//!   template, its status and how long it took ([`access_log`]). The route
//!   is the template (`/api/v1/datasets/{radargram_id}`), never the path, so
//!   the log neither grows a line shape per radargram nor carries ids, and
//!   it does not name the caller (see "Server logging" in `ARCHITECTURE.md`).
//! - **A heartbeat**: every `--heartbeat-seconds`, one line of what only
//!   Ridal knows -- render permits, the render cache, requests served and
//!   the processing queue -- beside the process's own CPU time and memory
//!   ([`Heartbeat`]). Host-wide numbers are left to `htop` and its kin.
//!
//! The banner `launch.rs` prints at startup stays `println!`: it is for the
//! person starting the server, not for a log.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::{MatchedPath, Request, State};
use axum::middleware::Next;
use axum::response::Response;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::EnvFilter;

use super::site::SiteState;

/// What is logged when `RUST_LOG` is unset: Ridal's own events from `info`,
/// its dependencies' only from `warn`.
pub const DEFAULT_FILTER: &str = "warn,ridal=info";

/// How often the heartbeat is written unless `--heartbeat-seconds` says.
pub const DEFAULT_HEARTBEAT_SECONDS: u64 = 60;

/// The access log's target, so `RUST_LOG=warn,ridal=info,ridal::access=warn`
/// silences it alone.
const ACCESS_TARGET: &str = "ridal::access";
/// The heartbeat's target, for the same reason.
const HEARTBEAT_TARGET: &str = "ridal::heartbeat";

/// The subscriber both server commands install, writing to `writer`.
///
/// Plain text: a JSON format for log shippers can come when somebody needs
/// one. Factored out of [`install`] so a test can capture exactly what a
/// server would write without touching the global subscriber.
pub fn subscriber<W>(
    filter: EnvFilter,
    writer: W,
    ansi: bool,
) -> impl tracing::Subscriber + Send + Sync
where
    W: for<'a> MakeWriter<'a> + Send + Sync + 'static,
{
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(writer)
        .with_ansi(ansi)
        .finish()
}

/// Install the server's subscriber on stderr, filtered by `RUST_LOG`.
///
/// A `RUST_LOG` that does not parse is said out loud and replaced by the
/// default, rather than leaving the server silent. One already installed
/// (a test, or an embedding program) is left in place.
pub fn install() {
    let filter = match std::env::var("RUST_LOG") {
        Ok(text) if !text.trim().is_empty() => EnvFilter::try_new(&text).unwrap_or_else(|e| {
            eprintln!("Warning: ignoring RUST_LOG '{text}' ({e}); using '{DEFAULT_FILTER}'");
            EnvFilter::new(DEFAULT_FILTER)
        }),
        _ => EnvFilter::new(DEFAULT_FILTER),
    };
    let ansi = std::io::IsTerminal::is_terminal(&std::io::stderr());
    let _ = tracing::subscriber::set_global_default(subscriber(filter, std::io::stderr, ansi));
}

/// Counts one waiter on a counter for as long as it lives.
///
/// A guard rather than an increment and a decrement, so a request that is
/// dropped while it waits -- the client went away -- stops counting too.
pub struct Waiting<'a>(&'a AtomicUsize);

impl<'a> Waiting<'a> {
    pub fn on(counter: &'a AtomicUsize) -> Self {
        counter.fetch_add(1, Ordering::Relaxed);
        Self(counter)
    }
}

impl Drop for Waiting<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

// ---------------------------------------------------------------------------
// Access log
// ---------------------------------------------------------------------------

/// The route a project's router matched, handed back to the site's access
/// log on the response.
///
/// The site router reaches a project through its fallback, so the site
/// never sees which project route answered. [`record_route`] runs inside
/// the project's router, where it does.
#[derive(Debug, Clone)]
pub struct RouteTemplate(Arc<str>);

/// Put the project route that answered on the response (#282).
pub async fn record_route(request: Request, next: Next) -> Response {
    let matched = request
        .extensions()
        .get::<MatchedPath>()
        .map(|path| RouteTemplate(Arc::from(path.as_str())));
    let mut response = next.run(request).await;
    if let Some(matched) = matched {
        response.extensions_mut().insert(matched);
    }
    response
}

/// One line per request: method, route template, status, milliseconds.
///
/// The time is to the response's headers. A large download streams its
/// body after that, so its line says how long the server took to start
/// answering, not how long the transfer took.
///
/// Static assets and image tiles are logged at `debug`: a viewer scrolling
/// along a radargram fetches tiles by the hundred, and those lines would
/// bury every other. They are still counted in the heartbeat's requests.
pub async fn access_log(
    State(site): State<Arc<SiteState>>,
    request: Request,
    next: Next,
) -> Response {
    let started = Instant::now();
    let method = request.method().clone();
    let path = request.uri().path().to_string();
    let matched = request
        .extensions()
        .get::<MatchedPath>()
        .map(|path| path.as_str().to_string());
    let response = next.run(request).await;
    site.count_request();

    let route = route_for_log(
        &path,
        matched.as_deref(),
        response.extensions().get::<RouteTemplate>().map(|t| &*t.0),
    );
    let status = response.status().as_u16();
    let ms = started.elapsed().as_secs_f64() * 1000.0;
    let ms = (ms * 10.0).round() / 10.0;
    if is_bulk(&route) {
        tracing::debug!(target: ACCESS_TARGET, method = %method, route = %route, status, ms, "request");
    } else {
        tracing::info!(target: ACCESS_TARGET, method = %method, route = %route, status, ms, "request");
    }
    response
}

/// The template a request is logged under.
///
/// A site route is its own template. A project route is the project
/// router's template under the site's prefix, with the project key as
/// `{key}` like every other path parameter. Anything that matched no route
/// is `unmatched`: the raw path would put whatever a scanner tried into
/// the log.
fn route_for_log(path: &str, site_route: Option<&str>, project_route: Option<&str>) -> String {
    if let Some(project_route) = project_route {
        if path.starts_with("/api/v1/projects/") {
            let tail = project_route
                .strip_prefix("/api/v1")
                .unwrap_or(project_route);
            return format!("/api/v1/projects/{{key}}{tail}");
        }
        if path.starts_with("/p/") {
            return format!("/p/{{key}}{project_route}");
        }
        return project_route.to_string();
    }
    match site_route {
        Some(route) => route.to_string(),
        None => "unmatched".to_string(),
    }
}

/// Routes fetched by the hundred per page, logged at `debug`.
fn is_bulk(route: &str) -> bool {
    route.starts_with("/static/")
        || route.ends_with("/chunks/{profile}/{x}/{y}")
        || route.ends_with("/overview")
}

// ---------------------------------------------------------------------------
// Heartbeat
// ---------------------------------------------------------------------------

/// One heartbeat's numbers, separate from how they are written so a test
/// can check them against the state they came from.
///
/// Counts marked "since the last" are read and reset by [`Heartbeat::take`],
/// so each line covers one interval. The site settings page reads the same
/// numbers with [`Heartbeat::peek`], which leaves them for the next line
/// (#391).
#[derive(Debug, Clone, PartialEq, serde::Serialize, utoipa::ToSchema)]
pub struct Heartbeat {
    /// How long the server has been running, in seconds.
    pub uptime_seconds: u64,
    /// How long the counts below marked "since the last" cover, in seconds:
    /// since the last heartbeat, or since the start when there is none.
    pub interval_seconds: f64,
    /// CPU time this process has used since it started, user and system,
    /// in seconds. `None` off Linux.
    #[schema(required = true)]
    pub cpu_seconds: Option<f64>,
    /// Resident memory, in MB. `None` off Linux.
    #[schema(required = true)]
    pub rss_mb: Option<f64>,
    /// `--n-workers`: renders that may run at once.
    pub render_permits: usize,
    /// Renders and merged exports running now.
    pub render_in_use: usize,
    /// Requests waiting for a render permit now.
    pub render_waiting: usize,
    /// What the render permits in use are doing, longest-running first
    /// (#473).
    pub renders: Vec<super::activity::RunningRender>,
    /// Images in the in-memory render cache, their size in MB, and
    /// `--cache-memory-mb`.
    pub cache_entries: usize,
    pub cache_mb: f64,
    pub cache_budget_mb: f64,
    /// Lookups that found their image, since the last heartbeat. `None`
    /// when there were none.
    #[schema(required = true)]
    pub cache_hit_ratio: Option<f64>,
    /// Requests answered since the last heartbeat.
    pub requests: u64,
    /// Processing jobs waiting and running now.
    pub jobs_queued: usize,
    pub jobs_running: usize,
    /// The running jobs' memory estimate against `--processing-memory-mb`.
    pub processing_mb: u64,
    pub processing_budget_mb: u64,
    /// How the jobs that ended since the last heartbeat ended.
    pub jobs: super::processing::FinishedCounts,
}

const MB: f64 = 1024.0 * 1024.0;

impl Heartbeat {
    /// Read the site's numbers, and start the interval counts again.
    pub fn take(site: &SiteState) -> Self {
        Self::read(site, true)
    }

    /// Read the site's numbers, leaving the interval counts to the next
    /// heartbeat.
    pub fn peek(site: &SiteState) -> Self {
        Self::read(site, false)
    }

    fn read(site: &SiteState, reset: bool) -> Self {
        let config = site.render_config();
        let render_permits = config.n_workers.max(1);
        let (hits, misses) = if reset {
            config.cache.take_lookups()
        } else {
            config.cache.lookups()
        };
        let lookups = hits + misses;
        let queue = site.processing.snapshot();
        let process = process_usage();
        Self {
            uptime_seconds: site.uptime().as_secs(),
            interval_seconds: round1(site.interval(reset).as_secs_f64()),
            cpu_seconds: process.map(|p| p.cpu_seconds),
            rss_mb: process.map(|p| round1(p.rss_bytes as f64 / MB)),
            render_permits,
            render_in_use: render_permits.saturating_sub(site.render_permits.available_permits()),
            render_waiting: config.permit_waiters.load(Ordering::Relaxed),
            renders: config.activity.snapshot(),
            cache_entries: config.cache.len(),
            cache_mb: round1(config.cache.current_bytes() as f64 / MB),
            cache_budget_mb: round1(config.cache.max_bytes() as f64 / MB),
            cache_hit_ratio: (lookups > 0)
                .then(|| (hits as f64 / lookups as f64 * 1000.0).round() / 1000.0),
            requests: if reset {
                site.take_requests()
            } else {
                site.requests()
            },
            jobs_queued: queue.queued,
            jobs_running: queue.running,
            processing_mb: queue.running_bytes / (1024 * 1024),
            processing_budget_mb: site.processing.config().memory_bytes / (1024 * 1024),
            jobs: if reset {
                site.processing.take_finished()
            } else {
                site.processing.finished()
            },
        }
    }

    /// Write this heartbeat as one `info` line.
    pub fn log(&self) {
        // The longest-running render only: a line has room for one, and it
        // is the one that says whether something is stuck.
        let longest = self.renders.first();
        let render_longest = longest.map(|render| {
            format!(
                "{}:{}/{}",
                render.kind.as_str(),
                render.project,
                render.target
            )
        });
        tracing::info!(
            target: HEARTBEAT_TARGET,
            cpu_s = self.cpu_seconds,
            rss_mb = self.rss_mb,
            render_in_use = self.render_in_use,
            render_permits = self.render_permits,
            render_waiting = self.render_waiting,
            render_longest = render_longest.as_deref(),
            render_longest_s = longest.map(|render| render.seconds),
            cache_entries = self.cache_entries,
            cache_mb = self.cache_mb,
            cache_budget_mb = self.cache_budget_mb,
            cache_hit_ratio = self.cache_hit_ratio,
            requests = self.requests,
            jobs_queued = self.jobs_queued,
            jobs_running = self.jobs_running,
            processing_mb = self.processing_mb,
            processing_budget_mb = self.processing_budget_mb,
            jobs_done = self.jobs.done,
            jobs_failed = self.jobs.failed,
            jobs_timed_out = self.jobs.timed_out,
            jobs_cancelled = self.jobs.cancelled,
            "heartbeat"
        );
    }
}

fn round1(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

/// Write a heartbeat every `every`, from the next interval on; nothing
/// when it is zero.
///
/// Must be called within a tokio runtime. The task ends with it.
pub fn spawn_heartbeat(
    site: Arc<SiteState>,
    every: Duration,
) -> Option<tokio::task::JoinHandle<()>> {
    if every.is_zero() {
        return None;
    }
    Some(tokio::spawn(async move {
        let mut ticks = tokio::time::interval(every);
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // The first tick is immediate, and a heartbeat at startup says
        // nothing the banner has not.
        ticks.tick().await;
        loop {
            ticks.tick().await;
            Heartbeat::take(&site).log();
        }
    }))
}

/// This process's CPU time and resident memory.
#[derive(Debug, Clone, Copy, PartialEq)]
struct ProcessUsage {
    cpu_seconds: f64,
    rss_bytes: u64,
}

/// Read from `/proc/self` on Linux, which costs no dependency; `None`
/// elsewhere, where the heartbeat leaves the two fields out.
#[cfg(target_os = "linux")]
fn process_usage() -> Option<ProcessUsage> {
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    // SAFETY: a read-only query of a configuration value.
    let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if ticks <= 0 {
        return None;
    }
    Some(ProcessUsage {
        cpu_seconds: round1(cpu_ticks(&stat)? as f64 / ticks as f64),
        rss_bytes: rss_kib(&status)? * 1024,
    })
}

#[cfg(not(target_os = "linux"))]
fn process_usage() -> Option<ProcessUsage> {
    None
}

/// User plus system CPU time, in clock ticks, from `/proc/self/stat`.
///
/// Counted from after the command name, which is in parentheses and may
/// itself hold spaces and parentheses: `utime` and `stat` are fields 14
/// and 15, and the state after the name is field 3.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn cpu_ticks(stat: &str) -> Option<u64> {
    let after_name = &stat[stat.rfind(')')? + 1..];
    let fields: Vec<&str> = after_name.split_whitespace().collect();
    let utime: u64 = fields.get(14 - 3)?.parse().ok()?;
    let stime: u64 = fields.get(15 - 3)?.parse().ok()?;
    Some(utime + stime)
}

/// `VmRSS` from `/proc/self/status`, in KiB.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn rss_kib(status: &str) -> Option<u64> {
    status
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

/// A writer that keeps what is written, for tests that read the log.
#[cfg(test)]
#[derive(Clone, Default)]
pub struct CapturedLog(Arc<std::sync::Mutex<Vec<u8>>>);

#[cfg(test)]
impl CapturedLog {
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }

    /// Install [`subscriber`] with the default filter on this thread for
    /// as long as the guard lives, writing here.
    pub fn install(&self) -> tracing::subscriber::DefaultGuard {
        tracing::subscriber::set_default(subscriber(
            EnvFilter::new(DEFAULT_FILTER),
            self.clone(),
            false,
        ))
    }
}

#[cfg(test)]
impl std::io::Write for CapturedLog {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
impl<'a> MakeWriter<'a> for CapturedLog {
    type Writer = CapturedLog;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_time_is_read_after_a_command_name_with_spaces_in_it() {
        // Fields 14 and 15 are 7 and 3; the name holds both a space and a
        // closing parenthesis, which a split on whitespace would miscount.
        let stat = "1234 (a b) c) S 1 2 3 4 5 6 7 8 9 10 7 3 0 0 20 0 1 0 100";
        assert_eq!(cpu_ticks(stat), Some(10));
        assert_eq!(cpu_ticks("garbage"), None);
    }

    #[test]
    fn resident_memory_is_read_from_vmrss() {
        let status = "Name:\tridal\nVmPeak:\t  900 kB\nVmRSS:\t  4321 kB\nThreads:\t8\n";
        assert_eq!(rss_kib(status), Some(4321));
        assert_eq!(rss_kib("Name:\tridal\n"), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn this_process_reports_its_usage_on_linux() {
        let usage = process_usage().expect("/proc/self is readable");
        assert!(usage.rss_bytes > 0);
        assert!(usage.cpu_seconds >= 0.0);
    }

    #[test]
    fn a_project_route_is_logged_under_the_site_prefix_with_its_key_as_a_parameter() {
        assert_eq!(
            route_for_log(
                "/api/v1/projects/dronbreen/datasets/line-01",
                None,
                Some("/api/v1/datasets/{radargram_id}"),
            ),
            "/api/v1/projects/{key}/datasets/{radargram_id}"
        );
        assert_eq!(
            route_for_log(
                "/p/default/view/line-01",
                None,
                Some("/view/{radargram_id}")
            ),
            "/p/{key}/view/{radargram_id}"
        );
        assert_eq!(
            route_for_log(
                "/api/v1/accounts/erik",
                Some("/api/v1/accounts/{name}"),
                None
            ),
            "/api/v1/accounts/{name}"
        );
        assert_eq!(route_for_log("/wp-login.php", None, None), "unmatched");
    }

    #[test]
    fn tiles_and_assets_are_bulk_and_pages_are_not() {
        assert!(is_bulk("/static/app.js"));
        assert!(is_bulk(
            "/p/{key}/api/v1/datasets/{radargram_id}/views/{view}/chunks/{profile}/{x}/{y}"
        ));
        assert!(!is_bulk("/p/{key}/view/{radargram_id}"));
    }

    #[test]
    fn a_waiter_stops_counting_when_it_is_dropped() {
        let counter = AtomicUsize::new(0);
        {
            let _one = Waiting::on(&counter);
            let _two = Waiting::on(&counter);
            assert_eq!(counter.load(Ordering::Relaxed), 2);
        }
        assert_eq!(counter.load(Ordering::Relaxed), 0);
    }
}
