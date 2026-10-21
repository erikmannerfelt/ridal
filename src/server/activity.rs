//! The renders running now, for the status page and the heartbeat (#473).
//!
//! A render permit says only that a worker is busy. When the server stopped
//! rendering in #472, telling which radargram and which kind of render held
//! the workers, and for how long, took a debugger. Every render that holds
//! a permit registers here for as long as it holds it.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde::Serialize;
use utoipa::ToSchema;

use crate::identity::ProjectKey;

/// What a render permit is being used for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RenderKind {
    /// One viewer chunk.
    Chunk,
    /// A low-resolution image of a whole radargram.
    Overview,
    /// An annotated figure export.
    Figure,
    /// One trace's amplitudes, for the trace view.
    Trace,
    /// A merged level 2 download of a group or the catalog.
    Level2,
}

impl RenderKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Chunk => "chunk",
            Self::Overview => "overview",
            Self::Figure => "figure",
            Self::Trace => "trace",
            Self::Level2 => "level2",
        }
    }
}

/// One render holding a permit now.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct RunningRender {
    pub kind: RenderKind,
    pub project: ProjectKey,
    /// What it renders: a radargram's id, `draft N` for processing job
    /// `N`'s draft, or `group G` or `catalog` for a merged level 2 export.
    pub target: String,
    /// Since it took its render permit, in seconds.
    pub seconds: f64,
    /// Whether it is still waiting for its radargram's lock, behind another
    /// render of the same radargram, rather than rendering. Never for a
    /// level 2 export, which takes no radargram's lock.
    pub waiting_for_radargram: bool,
}

#[derive(Debug)]
struct Entry {
    kind: RenderKind,
    project: ProjectKey,
    target: String,
    started: Instant,
    rendering: bool,
}

#[derive(Debug, Default)]
struct Registry {
    next: u64,
    running: BTreeMap<u64, Entry>,
}

/// The server's running renders. Shared by every clone, so one site has
/// one list across its projects.
#[derive(Debug, Clone, Default)]
pub struct RenderActivity(Arc<Mutex<Registry>>);

impl RenderActivity {
    /// Register a render that has just taken its permit. It stays listed
    /// until the guard is dropped.
    pub fn start(
        &self,
        kind: RenderKind,
        project: &ProjectKey,
        target: impl Into<String>,
    ) -> ActivityGuard {
        let mut registry = self.lock();
        let id = registry.next;
        registry.next += 1;
        registry.running.insert(
            id,
            Entry {
                kind,
                project: project.clone(),
                target: target.into(),
                started: Instant::now(),
                rendering: false,
            },
        );
        ActivityGuard {
            activity: self.clone(),
            id,
        }
    }

    /// The running renders, longest-running first.
    pub fn snapshot(&self) -> Vec<RunningRender> {
        let now = Instant::now();
        let registry = self.lock();
        let mut entries: Vec<&Entry> = registry.running.values().collect();
        entries.sort_by_key(|entry| entry.started);
        entries
            .into_iter()
            .map(|entry| RunningRender {
                kind: entry.kind,
                project: entry.project.clone(),
                target: entry.target.clone(),
                seconds: (now.duration_since(entry.started).as_secs_f64() * 10.0).round() / 10.0,
                waiting_for_radargram: !entry.rendering,
            })
            .collect()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Registry> {
        // Every critical section here is a map insert, update or read, none
        // of which can panic halfway, so a poisoned lock still holds a
        // consistent map.
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Keeps a render listed in its [`RenderActivity`] until dropped.
#[derive(Debug)]
pub struct ActivityGuard {
    activity: RenderActivity,
    id: u64,
}

impl ActivityGuard {
    /// The render has its radargram's lock and is rendering.
    pub fn rendering(&self) {
        if let Some(entry) = self.activity.lock().running.get_mut(&self.id) {
            entry.rendering = true;
        }
    }
}

impl Drop for ActivityGuard {
    fn drop(&mut self) {
        self.activity.lock().running.remove(&self.id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids() -> (ProjectKey, &'static str) {
        (ProjectKey::new("glaciers").unwrap(), "dronbreen-1")
    }

    #[test]
    fn a_render_is_listed_while_its_guard_lives() {
        let activity = RenderActivity::default();
        let (project, radargram) = ids();
        let guard = activity.start(RenderKind::Figure, &project, radargram);

        let renders = activity.snapshot();
        assert_eq!(renders.len(), 1);
        assert_eq!(renders[0].kind, RenderKind::Figure);
        assert!(renders[0].waiting_for_radargram);

        guard.rendering();
        assert!(!activity.snapshot()[0].waiting_for_radargram);

        drop(guard);
        assert!(activity.snapshot().is_empty());
    }

    #[test]
    fn the_longest_running_render_comes_first() {
        let activity = RenderActivity::default();
        let (project, radargram) = ids();
        let _first = activity.start(RenderKind::Overview, &project, radargram);
        std::thread::sleep(std::time::Duration::from_millis(5));
        let _second = activity.start(RenderKind::Chunk, &project, radargram);
        let kinds: Vec<_> = activity.snapshot().iter().map(|r| r.kind).collect();
        assert_eq!(kinds, [RenderKind::Overview, RenderKind::Chunk]);
    }
}
