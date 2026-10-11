//! The site-wide processing queue (#382, phase 3).
//!
//! Every processing job runs as a child process, `ridal process-job`
//! ([`crate::job`]), never in the server's own. A job that crashes, runs out
//! of memory or is killed takes nothing with it, and it does not take the
//! NetCDF lock that every render needs.
//!
//! What keeps a server from being overwhelmed, all set by the command line
//! ([`ProcessingConfig`]):
//!
//! - at most `jobs` children at once, site-wide;
//! - a memory budget: a job is admitted by its estimate, one larger than
//!   the whole budget is refused when it is submitted rather than queued
//!   forever, and jobs that together would exceed it wait;
//! - `threads_per_job` for each child's thread pool, so jobs × threads
//!   fits the machine;
//! - a timeout, after which the child is killed;
//! - at most `max_queued_per_user` unfinished jobs per person.
//!
//! Drafts go before batch items, so a long batch never makes someone wait
//! for a draft longer than the item already running. Two requests with one
//! key (the revision they would produce) are one job.
//!
//! There is no scheduler loop: [`ProcessingQueue::dispatch`] starts what can
//! start, and runs whenever a job is submitted or finishes.

// Nothing submits a job yet: the draft and batch routes of phase 5 (#382)
// are its first callers. Remove this when they land.
#![cfg_attr(not(test), allow(dead_code))]

use std::collections::BTreeMap;
use std::io::BufRead;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;

use crate::job::{Event, JobSpec};

/// How much processing a server takes on. See the module documentation.
#[derive(Debug, Clone)]
pub struct ProcessingConfig {
    pub jobs: usize,
    pub memory_bytes: u64,
    pub threads_per_job: usize,
    pub timeout: Duration,
    pub max_queued_per_user: usize,
    /// What runs a job: this executable, as `<program> process-job <spec>`.
    pub program: PathBuf,
    /// Where job specs are written while their jobs run.
    pub work_dir: PathBuf,
    /// How long a finished job's status is kept for its owner to read.
    pub keep_finished: Duration,
}

pub const DEFAULT_PROCESSING_JOBS: usize = 1;
pub const DEFAULT_PROCESSING_MEMORY_MB: u64 = 4096;
pub const DEFAULT_PROCESSING_TIMEOUT: Duration = Duration::from_secs(30 * 60);
pub const DEFAULT_MAX_QUEUED_PER_USER: usize = 5;

impl Default for ProcessingConfig {
    fn default() -> Self {
        Self {
            jobs: DEFAULT_PROCESSING_JOBS,
            memory_bytes: DEFAULT_PROCESSING_MEMORY_MB * 1024 * 1024,
            threads_per_job: std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(1),
            timeout: DEFAULT_PROCESSING_TIMEOUT,
            max_queued_per_user: DEFAULT_MAX_QUEUED_PER_USER,
            program: std::env::current_exe().unwrap_or_else(|_| PathBuf::from("ridal")),
            work_dir: std::env::temp_dir().join(format!("ridal-jobs-{}", std::process::id())),
            keep_finished: Duration::from_secs(60 * 60),
        }
    }
}

/// Which jobs go first. Declared in that order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    /// Someone is waiting to look at it.
    Draft,
    /// One radargram of a batch.
    Batch,
}

pub struct JobRequest {
    /// Two requests with one key are one job: the revision id the job
    /// would produce, once there are drafts.
    pub key: String,
    pub owner: String,
    pub priority: Priority,
    pub spec: JobSpec,
    /// How much memory the job is expected to use, in bytes.
    pub estimate_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum JobState {
    Queued,
    Running,
    Done { revision_id: String },
    Failed { message: String },
    Cancelled,
    TimedOut,
}

impl JobState {
    pub fn is_finished(&self) -> bool {
        !matches!(self, JobState::Queued | JobState::Running)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StepProgress {
    /// From 0.
    pub index: usize,
    pub of: usize,
    pub step: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct JobStatus {
    pub id: u64,
    pub key: String,
    pub owner: String,
    pub priority: Priority,
    #[serde(flatten)]
    pub state: JobState,
    /// The step running now, or the last one that ran.
    pub step: Option<StepProgress>,
    pub output: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubmitError {
    /// The owner already has this many unfinished jobs.
    TooMany { limit: usize },
    /// The job's estimate exceeds the whole memory budget.
    TooLarge {
        estimate_bytes: u64,
        budget_bytes: u64,
    },
}

impl std::fmt::Display for SubmitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SubmitError::TooMany { limit } => write!(
                f,
                "You already have {limit} processing jobs waiting or running; wait for \
                 one to finish or cancel one."
            ),
            SubmitError::TooLarge {
                estimate_bytes,
                budget_bytes,
            } => write!(
                f,
                "This job needs about {} MB, more than the {} MB this server allows \
                 processing to use at once.",
                estimate_bytes / (1024 * 1024),
                budget_bytes / (1024 * 1024)
            ),
        }
    }
}

/// Counts for a heartbeat or a status page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct QueueSnapshot {
    pub queued: usize,
    pub running: usize,
    pub running_bytes: u64,
}

struct Job {
    status: JobStatus,
    estimate_bytes: u64,
    /// Taken when the job starts.
    spec: Option<JobSpec>,
    cancel: Arc<tokio::sync::Notify>,
    cancelled: bool,
    finished_at: Option<std::time::Instant>,
}

#[derive(Default)]
struct Inner {
    next_id: u64,
    jobs: BTreeMap<u64, Job>,
}

pub struct ProcessingQueue {
    config: ProcessingConfig,
    inner: Mutex<Inner>,
}

impl ProcessingQueue {
    pub fn new(config: ProcessingConfig) -> Arc<Self> {
        Arc::new(Self {
            config,
            inner: Mutex::new(Inner::default()),
        })
    }

    /// Queue a job, or join the unfinished one with the same key.
    ///
    /// Must be called within a tokio runtime, which runs the job.
    pub fn submit(self: &Arc<Self>, request: JobRequest) -> Result<u64, SubmitError> {
        let id = {
            let mut inner = self.inner.lock().expect("the queue lock is not poisoned");
            if let Some((id, _)) = inner
                .jobs
                .iter()
                .find(|(_, job)| job.status.key == request.key && !job.status.state.is_finished())
            {
                return Ok(*id);
            }
            if request.estimate_bytes > self.config.memory_bytes {
                return Err(SubmitError::TooLarge {
                    estimate_bytes: request.estimate_bytes,
                    budget_bytes: self.config.memory_bytes,
                });
            }
            let unfinished = inner
                .jobs
                .values()
                .filter(|job| job.status.owner == request.owner && !job.status.state.is_finished())
                .count();
            if unfinished >= self.config.max_queued_per_user {
                return Err(SubmitError::TooMany {
                    limit: self.config.max_queued_per_user,
                });
            }
            inner.next_id += 1;
            let id = inner.next_id;
            inner.jobs.insert(
                id,
                Job {
                    status: JobStatus {
                        id,
                        key: request.key,
                        owner: request.owner,
                        priority: request.priority,
                        state: JobState::Queued,
                        step: None,
                        output: request.spec.output.clone(),
                    },
                    estimate_bytes: request.estimate_bytes,
                    spec: Some(request.spec),
                    cancel: Arc::new(tokio::sync::Notify::new()),
                    cancelled: false,
                    finished_at: None,
                },
            );
            id
        };
        self.dispatch();
        Ok(id)
    }

    pub fn status(&self, id: u64) -> Option<JobStatus> {
        let inner = self.inner.lock().expect("the queue lock is not poisoned");
        inner.jobs.get(&id).map(|job| job.status.clone())
    }

    /// Every job the queue still knows of, oldest first.
    pub fn list(&self) -> Vec<JobStatus> {
        let inner = self.inner.lock().expect("the queue lock is not poisoned");
        inner.jobs.values().map(|job| job.status.clone()).collect()
    }

    pub fn snapshot(&self) -> QueueSnapshot {
        let inner = self.inner.lock().expect("the queue lock is not poisoned");
        let mut snapshot = QueueSnapshot {
            queued: 0,
            running: 0,
            running_bytes: 0,
        };
        for job in inner.jobs.values() {
            match job.status.state {
                JobState::Queued => snapshot.queued += 1,
                JobState::Running => {
                    snapshot.running += 1;
                    snapshot.running_bytes += job.estimate_bytes;
                }
                _ => {}
            }
        }
        snapshot
    }

    /// Cancel a job: a queued one never starts, a running one is killed.
    /// `false` when it had already finished, or never existed.
    pub fn cancel(self: &Arc<Self>, id: u64) -> bool {
        let mut inner = self.inner.lock().expect("the queue lock is not poisoned");
        let Some(job) = inner.jobs.get_mut(&id) else {
            return false;
        };
        match job.status.state {
            JobState::Queued => {
                job.status.state = JobState::Cancelled;
                job.spec = None;
                job.finished_at = Some(std::time::Instant::now());
                true
            }
            JobState::Running => {
                job.cancelled = true;
                job.cancel.notify_one();
                true
            }
            _ => false,
        }
    }

    /// Start every queued job that fits, in priority order, and forget
    /// finished jobs older than `keep_finished`.
    fn dispatch(self: &Arc<Self>) {
        let mut inner = self.inner.lock().expect("the queue lock is not poisoned");
        let keep = self.config.keep_finished;
        inner
            .jobs
            .retain(|_, job| job.finished_at.is_none_or(|at| at.elapsed() < keep));

        let mut running = 0;
        let mut running_bytes = 0;
        for job in inner.jobs.values() {
            if job.status.state == JobState::Running {
                running += 1;
                running_bytes += job.estimate_bytes;
            }
        }
        let mut waiting: Vec<(Priority, u64)> = inner
            .jobs
            .values()
            .filter(|job| job.status.state == JobState::Queued)
            .map(|job| (job.status.priority, job.status.id))
            .collect();
        waiting.sort();

        for (_, id) in waiting {
            if running >= self.config.jobs.max(1) {
                break;
            }
            let job = inner.jobs.get_mut(&id).expect("listed just now");
            // Strictly in order: a large job at the front is not overtaken
            // by smaller ones behind it forever.
            if running > 0 && running_bytes + job.estimate_bytes > self.config.memory_bytes {
                break;
            }
            let Some(spec) = job.spec.take() else {
                continue;
            };
            job.status.state = JobState::Running;
            running += 1;
            running_bytes += job.estimate_bytes;
            let cancel = job.cancel.clone();
            let queue = Arc::clone(self);
            tokio::spawn(async move {
                let state = queue.run(id, spec, cancel).await;
                queue.finish(id, state);
                queue.dispatch();
            });
        }
    }

    fn finish(&self, id: u64, state: JobState) {
        let mut inner = self.inner.lock().expect("the queue lock is not poisoned");
        if let Some(job) = inner.jobs.get_mut(&id) {
            job.status.state = if job.cancelled {
                JobState::Cancelled
            } else {
                state
            };
            job.finished_at = Some(std::time::Instant::now());
        }
    }

    fn progress(&self, id: u64, step: StepProgress) {
        let mut inner = self.inner.lock().expect("the queue lock is not poisoned");
        if let Some(job) = inner.jobs.get_mut(&id) {
            job.status.step = Some(step);
        }
    }

    /// Run one job to its end, and say how it ended.
    async fn run(
        self: &Arc<Self>,
        id: u64,
        spec: JobSpec,
        cancel: Arc<tokio::sync::Notify>,
    ) -> JobState {
        let failed = |message: String| JobState::Failed { message };
        if let Err(e) = std::fs::create_dir_all(&self.config.work_dir) {
            return failed(format!("could not prepare to run the job: {e}"));
        }
        let spec_path = self.config.work_dir.join(format!("job-{id}.json"));
        let text = serde_json::to_string(&spec).expect("a job spec always serializes");
        if let Err(e) = std::fs::write(&spec_path, text) {
            return failed(format!("could not prepare to run the job: {e}"));
        }

        let mut command = std::process::Command::new(&self.config.program);
        command
            .arg("process-job")
            .arg(&spec_path)
            .env(
                "RAYON_NUM_THREADS",
                self.config.threads_per_job.max(1).to_string(),
            )
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut child = match crate::tools::spawn_tool(&mut command) {
            Ok(child) => child,
            Err(e) => {
                let _ = std::fs::remove_file(&spec_path);
                return failed(format!(
                    "could not start {}: {e}",
                    self.config.program.display()
                ));
            }
        };
        let stdout = child.stdout.take().expect("stdout is piped");
        let stderr = child.stderr.take().expect("stderr is piped");
        let child = Arc::new(Mutex::new(child));

        // Read on blocking threads: the child's pipes are std pipes.
        let queue = Arc::clone(self);
        let events = tokio::task::spawn_blocking(move || {
            let mut last = None;
            for line in std::io::BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                match serde_json::from_str::<Event>(&line) {
                    Ok(Event::Step { index, of, step }) => {
                        queue.progress(id, StepProgress { index, of, step })
                    }
                    Ok(event) => last = Some(event),
                    // Anything else a step printed is not an event.
                    Err(_) => {}
                }
            }
            last
        });
        let errors = tokio::task::spawn_blocking(move || {
            // The end of stderr is what explains a crash; keep a little.
            let mut tail = std::collections::VecDeque::new();
            for line in std::io::BufReader::new(stderr)
                .lines()
                .map_while(Result::ok)
            {
                if tail.len() == 20 {
                    tail.pop_front();
                }
                tail.push_back(line);
            }
            tail.into_iter().collect::<Vec<_>>().join("\n")
        });

        let kill = |child: &Arc<Mutex<std::process::Child>>| {
            if let Ok(mut child) = child.lock() {
                let _ = child.kill();
            }
        };
        let mut events = events;
        let stopped = tokio::select! {
            last = &mut events => Ok(last.ok().flatten()),
            _ = tokio::time::sleep(self.config.timeout) => Err(JobState::TimedOut),
            _ = cancel.notified() => Err(JobState::Cancelled),
        };
        let last = match stopped {
            Ok(last) => last,
            Err(state) => {
                kill(&child);
                // A killed job's pipes close when it does, unless something
                // it started still holds them; that must not hold this
                // slot, so its output is only waited for briefly.
                let grace = Duration::from_secs(2);
                let _ = tokio::time::timeout(grace, &mut events).await;
                let _ = tokio::time::timeout(grace, errors).await;
                let _ = tokio::task::spawn_blocking(move || {
                    if let Ok(mut child) = child.lock() {
                        let _ = child.wait();
                    }
                })
                .await;
                let _ = std::fs::remove_file(&spec_path);
                return state;
            }
        };
        let stderr = errors.await.unwrap_or_default();
        let exit = tokio::task::spawn_blocking(move || child.lock().ok().map(|mut c| c.wait()))
            .await
            .ok()
            .flatten();
        let _ = std::fs::remove_file(&spec_path);

        match (last, exit) {
            (Some(Event::Done { revision_id, .. }), Some(Ok(status))) if status.success() => {
                JobState::Done { revision_id }
            }
            (Some(Event::Failed { message }), _) => failed(message),
            (_, Some(Ok(status))) => failed(if stderr.is_empty() {
                format!("the job stopped without finishing ({status})")
            } else {
                format!("the job stopped without finishing ({status}): {stderr}")
            }),
            (_, _) => failed("the job stopped without finishing".to_string()),
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// A queue whose jobs are `script`, a shell script standing in for
    /// `ridal process-job`: what the queue does with a child does not
    /// depend on what the child is.
    fn queue(
        dir: &std::path::Path,
        script: &str,
        config: ProcessingConfig,
    ) -> Arc<ProcessingQueue> {
        use std::os::unix::fs::PermissionsExt;
        let program = dir.join("job.sh");
        std::fs::write(&program, format!("#!/bin/sh\n{script}\n")).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        ProcessingQueue::new(ProcessingConfig {
            program,
            work_dir: dir.join("work"),
            ..config
        })
    }

    fn request(key: &str, owner: &str, priority: Priority) -> JobRequest {
        JobRequest {
            key: key.to_string(),
            owner: owner.to_string(),
            priority,
            spec: JobSpec {
                inputs: vec![],
                output: PathBuf::from(format!("{key}.nc")),
                steps: vec![],
                medium_velocity: 0.168,
                crs: None,
                dem: None,
                cor: None,
                antenna_mhz_override: None,
                antenna_separation_override: None,
                radargram_id: key.to_string(),
                display_name: None,
                group_name: None,
                group_id: None,
            },
            estimate_bytes: 1,
        }
    }

    async fn finished(queue: &ProcessingQueue, id: u64) -> JobStatus {
        for _ in 0..400 {
            let status = queue.status(id).unwrap();
            if status.state.is_finished() {
                return status;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("job {id} did not finish: {:?}", queue.status(id));
    }

    const DONE: &str = r#"
echo '{"event":"step","index":0,"of":2,"step":"dewow"}'
echo 'something a step printed'
echo '{"event":"step","index":1,"of":2,"step":"gain(factor=1)"}'
echo '{"event":"exporting"}'
echo '{"event":"done","output":"x.nc","revision_id":"abc"}'
"#;

    #[tokio::test]
    async fn a_job_runs_to_its_revision_and_reports_its_steps() {
        let dir = tempfile::tempdir().unwrap();
        let queue = queue(dir.path(), DONE, ProcessingConfig::default());
        let id = queue.submit(request("a", "anna", Priority::Draft)).unwrap();
        let status = finished(&queue, id).await;
        assert_eq!(
            status.state,
            JobState::Done {
                revision_id: "abc".to_string()
            }
        );
        assert_eq!(status.step.unwrap().index, 1);
        // The spec file is gone with the job.
        assert_eq!(
            std::fs::read_dir(dir.path().join("work")).unwrap().count(),
            0
        );
    }

    #[tokio::test]
    async fn a_failure_says_why_whether_or_not_the_job_could() {
        let dir = tempfile::tempdir().unwrap();
        let said = queue(
            dir.path(),
            r#"echo '{"event":"failed","message":"subset out of bounds"}'; exit 1"#,
            ProcessingConfig::default(),
        );
        let id = said.submit(request("a", "anna", Priority::Draft)).unwrap();
        assert_eq!(
            finished(&said, id).await.state,
            JobState::Failed {
                message: "subset out of bounds".to_string()
            }
        );

        let crashed = queue(
            dir.path(),
            "echo boom >&2; exit 3",
            ProcessingConfig::default(),
        );
        let id = crashed
            .submit(request("b", "anna", Priority::Draft))
            .unwrap();
        match finished(&crashed, id).await.state {
            JobState::Failed { message } => assert!(message.contains("boom"), "{message}"),
            other => panic!("{other:?}"),
        }

        // Done, but exited badly: not done.
        let lied = queue(
            dir.path(),
            r#"echo '{"event":"done","output":"x.nc","revision_id":"abc"}'; exit 1"#,
            ProcessingConfig::default(),
        );
        let id = lied.submit(request("c", "anna", Priority::Draft)).unwrap();
        assert!(matches!(
            finished(&lied, id).await.state,
            JobState::Failed { .. }
        ));
    }

    #[tokio::test]
    async fn a_job_past_its_timeout_is_killed() {
        let dir = tempfile::tempdir().unwrap();
        let queue = queue(
            dir.path(),
            "exec sleep 30",
            ProcessingConfig {
                timeout: Duration::from_millis(200),
                ..ProcessingConfig::default()
            },
        );
        let started = std::time::Instant::now();
        let id = queue.submit(request("a", "anna", Priority::Draft)).unwrap();
        assert_eq!(finished(&queue, id).await.state, JobState::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[tokio::test]
    async fn a_killed_job_does_not_wait_for_what_it_started() {
        // `sleep` is the shell's child here and keeps the pipes open after
        // the shell is killed; the slot is freed anyway.
        let dir = tempfile::tempdir().unwrap();
        let queue = queue(
            dir.path(),
            "sleep 5; echo late",
            ProcessingConfig {
                timeout: Duration::from_millis(100),
                ..ProcessingConfig::default()
            },
        );
        let started = std::time::Instant::now();
        let id = queue.submit(request("a", "anna", Priority::Draft)).unwrap();
        assert_eq!(finished(&queue, id).await.state, JobState::TimedOut);
        assert!(
            started.elapsed() < Duration::from_secs(8),
            "{:?}",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn a_cancelled_job_stops_whether_queued_or_running() {
        let dir = tempfile::tempdir().unwrap();
        let queue = queue(dir.path(), "exec sleep 30", ProcessingConfig::default());
        let running = queue.submit(request("a", "anna", Priority::Draft)).unwrap();
        let queued = queue.submit(request("b", "anna", Priority::Draft)).unwrap();
        assert_eq!(queue.status(queued).unwrap().state, JobState::Queued);

        assert!(queue.cancel(queued));
        assert_eq!(queue.status(queued).unwrap().state, JobState::Cancelled);
        assert!(queue.cancel(running));
        assert_eq!(finished(&queue, running).await.state, JobState::Cancelled);
        assert!(!queue.cancel(running), "already finished");
        assert!(!queue.cancel(999));
    }

    #[tokio::test]
    async fn drafts_go_before_batch_items_and_only_jobs_at_once_run() {
        let dir = tempfile::tempdir().unwrap();
        // Each job appends its key when it starts, so the order is recorded
        // by the jobs themselves.
        let log = dir.path().join("order");
        let script = format!(
            "key=$(sed -n 's/.*\"radargram_id\":\"\\([^\"]*\\)\".*/\\1/p' \"$2\")\n\
             echo \"$key\" >> {}\nsleep 0.2\n{DONE}",
            log.display()
        );
        let queue = queue(dir.path(), &script, ProcessingConfig::default());
        let first = queue
            .submit(request("batch-1", "anna", Priority::Batch))
            .unwrap();
        queue
            .submit(request("batch-2", "anna", Priority::Batch))
            .unwrap();
        let draft = queue
            .submit(request("draft", "bert", Priority::Draft))
            .unwrap();
        assert_eq!(queue.snapshot().running, 1);
        assert_eq!(queue.snapshot().queued, 2);

        finished(&queue, first).await;
        finished(&queue, draft).await;
        for status in queue.list() {
            finished(&queue, status.id).await;
        }
        let order = std::fs::read_to_string(&log).unwrap();
        assert_eq!(
            order.lines().collect::<Vec<_>>(),
            ["batch-1", "draft", "batch-2"]
        );
    }

    #[tokio::test]
    async fn one_key_is_one_job() {
        let dir = tempfile::tempdir().unwrap();
        let queue = queue(dir.path(), "exec sleep 30", ProcessingConfig::default());
        let a = queue
            .submit(request("same", "anna", Priority::Draft))
            .unwrap();
        let b = queue
            .submit(request("same", "bert", Priority::Draft))
            .unwrap();
        assert_eq!(a, b);
        queue.cancel(a);
        finished(&queue, a).await;
        // Once it has finished, the key can run again.
        let c = queue
            .submit(request("same", "anna", Priority::Draft))
            .unwrap();
        assert_ne!(c, a);
        queue.cancel(c);
    }

    #[tokio::test]
    async fn the_limits_refuse_what_cannot_fit() {
        let dir = tempfile::tempdir().unwrap();
        let queue = queue(
            dir.path(),
            "exec sleep 30",
            ProcessingConfig {
                max_queued_per_user: 2,
                memory_bytes: 100,
                ..ProcessingConfig::default()
            },
        );
        let a = queue.submit(request("a", "anna", Priority::Draft)).unwrap();
        let b = queue.submit(request("b", "anna", Priority::Draft)).unwrap();
        assert_eq!(
            queue.submit(request("c", "anna", Priority::Draft)),
            Err(SubmitError::TooMany { limit: 2 })
        );
        // Someone else is not held to Anna's limit.
        let d = queue.submit(request("d", "bert", Priority::Draft)).unwrap();

        let mut huge = request("e", "carl", Priority::Draft);
        huge.estimate_bytes = 101;
        let refused = queue.submit(huge).unwrap_err();
        assert!(refused.to_string().contains("more than"), "{refused}");
        for id in [a, b, d] {
            queue.cancel(id);
        }
    }

    #[tokio::test]
    async fn jobs_that_together_exceed_the_memory_budget_wait() {
        let dir = tempfile::tempdir().unwrap();
        let queue = queue(
            dir.path(),
            "exec sleep 30",
            ProcessingConfig {
                jobs: 4,
                memory_bytes: 100,
                ..ProcessingConfig::default()
            },
        );
        let big = |key: &str| {
            let mut r = request(key, "anna", Priority::Draft);
            r.estimate_bytes = 60;
            queue.submit(r).unwrap()
        };
        let a = big("a");
        let b = big("b");
        assert_eq!(queue.snapshot().running, 1, "60 + 60 is over 100");
        assert_eq!(queue.snapshot().running_bytes, 60);
        queue.cancel(a);
        finished(&queue, a).await;
        for _ in 0..200 {
            if queue.status(b).unwrap().state == JobState::Running {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(queue.status(b).unwrap().state, JobState::Running);
        queue.cancel(b);
        finished(&queue, b).await;
    }
}
