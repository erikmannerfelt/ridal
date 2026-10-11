//! Processing profiles, and which radargrams use them (#382, phase 6).
//!
//! A **processing profile** is shared processing: a named list of steps and
//! a medium velocity, used by any number of radargrams across any number of
//! catalog groups. "Processed the same way" is a guarantee only because it
//! is one document. Each change makes a new version, and every version is
//! kept, so a radargram processed with version 3 of a profile that is now
//! at version 4 can say so.
//!
//! An **assignment** puts a radargram in a profile, optionally with steps
//! of its own that run first. Those may only choose traces and samples
//! ([`crate::steps::Step::selects_only`]): anything else would make the
//! radargram processed differently from its profile while still claiming
//! it.
//!
//! ```text
//! ridal_data/processing/
//!   profiles/<profile_id>.json   one profile, every version of it
//!   assignments.json             which radargram uses which profile
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::store::{DocumentStore, Expectation, StoreError, Version};
use crate::identity::{ProfileId, RadargramId};

const PROFILES: &str = "processing/profiles";
const ASSIGNMENTS: &str = "processing/assignments.json";

/// One version of a profile.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProfileVersion {
    /// From 1, one more for every change to the steps or the velocity.
    pub version: u32,
    /// Canonical, with every argument written out, as a recipe records
    /// them; so a changed default is not a silent change to the profile.
    pub steps: Vec<String>,
    /// m/ns.
    pub medium_velocity: f32,
    /// RFC 3339.
    pub at: String,
    /// Who made this version.
    pub by: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    pub id: ProfileId,
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// Oldest first; the last is current. Never empty.
    pub versions: Vec<ProfileVersion>,
}

impl Profile {
    pub fn current(&self) -> &ProfileVersion {
        self.versions
            .last()
            .expect("a profile always has a version")
    }
}

/// Which profile a radargram uses, and the steps of its own that run first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Assignment {
    pub profile: ProfileId,
    /// Canonical; selection steps only.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pre_steps: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Assignments {
    #[serde(default)]
    pub radargrams: BTreeMap<RadargramId, Assignment>,
}

impl Assignments {
    /// The radargrams using `profile`.
    pub fn members(&self, profile: &ProfileId) -> Vec<&RadargramId> {
        self.radargrams
            .iter()
            .filter(|(_, a)| &a.profile == profile)
            .map(|(id, _)| id)
            .collect()
    }
}

#[derive(Debug)]
pub enum ProcessingError {
    Store(StoreError),
    /// A document on disk that does not parse.
    Malformed {
        path: PathBuf,
        message: String,
    },
    /// Steps that are not valid, or a pre-step that does more than select.
    Steps(String),
    /// The velocity is not a positive, finite number.
    Velocity(f32),
    /// The profile is used and cannot be deleted.
    InUse {
        profile: ProfileId,
        radargrams: Vec<RadargramId>,
    },
    /// An assignment names a profile that does not exist.
    NoSuchProfile(ProfileId),
}

impl std::fmt::Display for ProcessingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProcessingError::Store(e) => write!(f, "{e}"),
            ProcessingError::Malformed { path, message } => {
                write!(f, "{} cannot be read: {message}", path.display())
            }
            ProcessingError::Steps(message) => write!(f, "{message}"),
            ProcessingError::Velocity(v) => {
                write!(
                    f,
                    "the medium velocity must be a positive number of m/ns, not {v}"
                )
            }
            ProcessingError::InUse {
                profile,
                radargrams,
            } => write!(
                f,
                "'{profile}' is used by {} radargram{} ({}); move them to another profile first",
                radargrams.len(),
                if radargrams.len() == 1 { "" } else { "s" },
                radargrams
                    .iter()
                    .take(5)
                    .map(|id| id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            ProcessingError::NoSuchProfile(id) => {
                write!(f, "there is no processing profile '{id}'")
            }
        }
    }
}

impl From<StoreError> for ProcessingError {
    fn from(e: StoreError) -> Self {
        ProcessingError::Store(e)
    }
}

/// `steps`, each checked and written out in full.
pub fn canonical_steps(steps: &[String]) -> Result<Vec<String>, ProcessingError> {
    steps
        .iter()
        .map(|step| {
            crate::steps::parse_one(step)
                .map(|parsed| parsed.canonical)
                .map_err(ProcessingError::Steps)
        })
        .collect()
}

/// `steps` as pre-steps: canonical, and each one that only selects.
pub fn canonical_pre_steps(steps: &[String]) -> Result<Vec<String>, ProcessingError> {
    steps
        .iter()
        .map(|step| {
            let parsed = crate::steps::parse_one(step).map_err(ProcessingError::Steps)?;
            if !parsed.step.selects_only() {
                return Err(ProcessingError::Steps(format!(
                    "`{}` cannot run before a profile's steps: a radargram's own steps \
                     may only choose traces and samples (`subset`, `remove_traces`). \
                     For different processing, use another profile.",
                    parsed.canonical
                )));
            }
            Ok(parsed.canonical)
        })
        .collect()
}

fn profile_path(id: &ProfileId) -> PathBuf {
    Path::new(PROFILES).join(format!("{}.json", id.as_str()))
}

pub fn read_profile(
    store: &DocumentStore,
    id: &ProfileId,
) -> Result<Option<(Profile, Version)>, ProcessingError> {
    let path = profile_path(id);
    let Some(document) = store.read(&path)? else {
        return Ok(None);
    };
    let profile = serde_json::from_str(&document.text).map_err(|e| ProcessingError::Malformed {
        path,
        message: e.to_string(),
    })?;
    Ok(Some((profile, document.version)))
}

/// Every profile, by id.
pub fn list_profiles(store: &DocumentStore) -> Result<Vec<Profile>, ProcessingError> {
    let mut out = Vec::new();
    for stem in store.list_stems(Path::new(PROFILES), ".json")? {
        let Ok(id) = ProfileId::new(&stem) else {
            continue;
        };
        if let Some((profile, _)) = read_profile(store, &id)? {
            out.push(profile);
        }
    }
    Ok(out)
}

/// What a client sends to create or change a profile.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ProfileEdit {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub steps: Vec<String>,
    pub medium_velocity: f32,
}

/// Create or change a profile, making a new version when the steps or the
/// velocity change. A new name or description alone is not a new version,
/// since it processes nothing differently.
pub fn save_profile(
    store: &DocumentStore,
    id: &ProfileId,
    edit: ProfileEdit,
    by: &str,
    at: &str,
    expected: &Expectation,
) -> Result<(Profile, Version), ProcessingError> {
    if !(edit.medium_velocity.is_finite() && edit.medium_velocity > 0.) {
        return Err(ProcessingError::Velocity(edit.medium_velocity));
    }
    let steps = canonical_steps(&edit.steps)?;
    let existing = read_profile(store, id)?;
    let mut versions = existing
        .as_ref()
        .map(|(profile, _)| profile.versions.clone())
        .unwrap_or_default();
    let changed = versions
        .last()
        .is_none_or(|v| v.steps != steps || v.medium_velocity != edit.medium_velocity);
    if changed {
        versions.push(ProfileVersion {
            version: versions.last().map_or(1, |v| v.version + 1),
            steps,
            medium_velocity: edit.medium_velocity,
            at: at.to_string(),
            by: by.to_string(),
        });
    }
    let profile = Profile {
        id: id.clone(),
        name: edit.name.trim().to_string(),
        description: edit.description.trim().to_string(),
        versions,
    };
    let text = serde_json::to_string_pretty(&profile).expect("a profile is plain data");
    let version = store.write(&profile_path(id), &text, expected)?;
    Ok((profile, version))
}

/// Delete a profile no radargram uses.
pub fn delete_profile(
    store: &DocumentStore,
    id: &ProfileId,
    expected: &Expectation,
) -> Result<bool, ProcessingError> {
    let (assignments, _) = read_assignments(store)?;
    let members: Vec<RadargramId> = assignments.members(id).into_iter().cloned().collect();
    if !members.is_empty() {
        return Err(ProcessingError::InUse {
            profile: id.clone(),
            radargrams: members,
        });
    }
    Ok(store.remove(&profile_path(id), expected)?)
}

/// The assignments, and what a write of them should expect to find.
pub fn read_assignments(
    store: &DocumentStore,
) -> Result<(Assignments, Expectation), ProcessingError> {
    let path = Path::new(ASSIGNMENTS);
    match store.read(path)? {
        None => Ok((Assignments::default(), Expectation::Absent)),
        Some(document) => {
            let assignments =
                serde_json::from_str(&document.text).map_err(|e| ProcessingError::Malformed {
                    path: path.to_path_buf(),
                    message: e.to_string(),
                })?;
            Ok((assignments, Expectation::Version(document.version)))
        }
    }
}

/// Put `radargram` in a profile, or with `None` take it out of any.
pub fn assign(
    store: &DocumentStore,
    radargram: &RadargramId,
    assignment: Option<(ProfileId, Vec<String>)>,
) -> Result<Option<Assignment>, ProcessingError> {
    let assignment = match assignment {
        None => None,
        Some((profile, pre_steps)) => {
            if read_profile(store, &profile)?.is_none() {
                return Err(ProcessingError::NoSuchProfile(profile));
            }
            Some(Assignment {
                profile,
                pre_steps: canonical_pre_steps(&pre_steps)?,
            })
        }
    };
    // One retry on a conflict, as the other shared documents do: two
    // people assigning different radargrams at once are both right.
    let mut attempts = 0;
    loop {
        attempts += 1;
        let (mut assignments, expected) = read_assignments(store)?;
        match &assignment {
            Some(a) => {
                assignments.radargrams.insert(radargram.clone(), a.clone());
            }
            None => {
                assignments.radargrams.remove(radargram);
            }
        }
        let text = serde_json::to_string_pretty(&assignments).expect("plain data");
        match store.write(Path::new(ASSIGNMENTS), &text, &expected) {
            Ok(_) => return Ok(assignment),
            Err(StoreError::Conflict { .. }) if attempts < 3 => continue,
            Err(e) => return Err(e.into()),
        }
    }
}

/// Where a radargram stands against its processing profile.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ProcessingState {
    /// Processed as the profile says now, from the raw data its source names.
    Current { profile_version: u32 },
    /// Processed some other way than the profile and source say now.
    Stale {
        /// The profile version it was processed with, when it matches one.
        profile_version: Option<u32>,
        reasons: Vec<String>,
    },
    /// Processed as the profile says, but by an older version of a step or
    /// of the rest of Ridal (#383). Not stale: the same request gives a
    /// different file only because Ridal has changed since.
    OlderProcessing {
        profile_version: u32,
        reasons: Vec<String>,
    },
    /// Raw data and a profile, but nothing processed yet.
    NotProcessed,
    /// No raw data in the project, so nothing can be reprocessed. Never
    /// counted as stale (#382).
    NoRawData,
}

/// The steps a radargram in `profile` is processed with at `version`.
fn expected_steps(assignment: &Assignment, version: &ProfileVersion) -> Vec<String> {
    assignment
        .pre_steps
        .iter()
        .chain(version.steps.iter())
        .cloned()
        .collect()
}

/// Where a radargram stands: `recipe` is what its served file records
/// (`None` with no file, or one without a recipe), `source` what the
/// project says it is made from.
pub fn state_of(
    assignment: &Assignment,
    profile: &Profile,
    source: Option<&super::raw::Source>,
    served: bool,
    recipe: Option<&crate::recipe::Recipe>,
) -> ProcessingState {
    let Some(source) = source else {
        return ProcessingState::NoRawData;
    };
    if !served {
        return ProcessingState::NotProcessed;
    }
    let current = profile.current();
    let Some(recipe) = recipe else {
        return ProcessingState::Stale {
            profile_version: None,
            reasons: vec![
                "processed before Ridal recorded what a file was made from, so it \
                 cannot be compared"
                    .to_string(),
            ],
        };
    };

    let steps: Vec<String> = recipe.steps.iter().map(|s| s.step.clone()).collect();
    let matched = profile
        .versions
        .iter()
        .rev()
        .find(|v| {
            expected_steps(assignment, v) == steps && v.medium_velocity == recipe.medium_velocity
        })
        .map(|v| v.version);

    let mut reasons = Vec::new();
    match matched {
        Some(v) if v == current.version => {}
        Some(v) => reasons.push(format!(
            "processed with version {v} of the profile, which is now at version {}",
            current.version
        )),
        None => reasons.push(
            "processed with steps or a medium velocity that match no version of the profile"
                .to_string(),
        ),
    }
    if recipe.bundles != source.bundles {
        reasons.push("its raw data has been switched since".to_string());
    }
    if recipe.dem.as_ref().map(|d| &d.blake3) != source.dem.as_ref().map(|d| &d.file.blake3) {
        reasons.push("its DEM has changed since".to_string());
    }
    if let Some(crs) = &source.crs {
        if crs != &recipe.crs {
            reasons.push(format!(
                "processed in {}, and its source now says {crs}",
                recipe.crs
            ));
        }
    }
    if !reasons.is_empty() {
        return ProcessingState::Stale {
            profile_version: matched,
            reasons,
        };
    }

    // The same request; is it the same Ridal?
    let mut older = Vec::new();
    if recipe.core_version < crate::recipe::CORE_PROCESSING_VERSION {
        older.push(format!(
            "reading and writing files has changed since (version {} → {})",
            recipe.core_version,
            crate::recipe::CORE_PROCESSING_VERSION
        ));
    }
    for step in &recipe.steps {
        if let Ok(parsed) = crate::steps::parse_one(&step.step) {
            let now = parsed.step.version();
            if step.version < now {
                let name = step.step.split('(').next().unwrap_or_default();
                older.push(format!(
                    "`{name}` has changed since (version {} → {now})",
                    step.version
                ));
            }
        }
    }
    if older.is_empty() {
        ProcessingState::Current {
            profile_version: current.version,
        }
    } else {
        ProcessingState::OlderProcessing {
            profile_version: current.version,
            reasons: older,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, DocumentStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = DocumentStore::new(dir.path().to_path_buf());
        (dir, store)
    }

    fn edit(steps: &[&str], velocity: f32) -> ProfileEdit {
        ProfileEdit {
            name: "Svalbard 100 MHz".to_string(),
            description: String::new(),
            steps: steps.iter().map(|s| s.to_string()).collect(),
            medium_velocity: velocity,
        }
    }

    fn id(name: &str) -> ProfileId {
        ProfileId::new(name).unwrap()
    }

    #[test]
    fn a_change_to_what_is_processed_is_a_new_version_and_a_new_name_is_not() {
        let (_dir, store) = store();
        let p = id("svalbard-100mhz");
        let (first, v1) = save_profile(
            &store,
            &p,
            edit(&["dewow", "gain(0.002)"], 0.168),
            "anna",
            "2026-10-10T12:00:00Z",
            &Expectation::Absent,
        )
        .unwrap();
        assert_eq!(first.current().version, 1);
        // Written out in full, as a recipe records them.
        assert!(
            first.current().steps[0].starts_with("dewow("),
            "{:?}",
            first.current().steps
        );

        let mut renamed = edit(&["dewow", "gain(0.002)"], 0.168);
        renamed.name = "Svalbard, 100 MHz antennas".to_string();
        let (renamed, v2) = save_profile(
            &store,
            &p,
            renamed,
            "bert",
            "2026-10-11T12:00:00Z",
            &Expectation::Version(v1.clone()),
        )
        .unwrap();
        assert_eq!(renamed.versions.len(), 1);

        let (changed, _) = save_profile(
            &store,
            &p,
            edit(&["dewow", "gain(0.003)"], 0.168),
            "bert",
            "2026-10-12T12:00:00Z",
            &Expectation::Version(v2),
        )
        .unwrap();
        assert_eq!(changed.current().version, 2);
        assert_eq!(changed.versions[0].by, "anna");
        assert_eq!(read_profile(&store, &p).unwrap().unwrap().0, changed);

        // A stale version is refused.
        let stale = save_profile(
            &store,
            &p,
            edit(&["dewow"], 0.168),
            "x",
            "t",
            &Expectation::Version(v1),
        );
        assert!(matches!(
            stale,
            Err(ProcessingError::Store(StoreError::Conflict { .. }))
        ));
    }

    #[test]
    fn bad_steps_and_velocities_are_refused() {
        let (_dir, store) = store();
        let p = id("p");
        let refused = save_profile(
            &store,
            &p,
            edit(&["dewoww"], 0.168),
            "a",
            "t",
            &Expectation::Any,
        );
        assert!(matches!(refused, Err(ProcessingError::Steps(_))));
        for velocity in [0., -0.1, f32::NAN] {
            let refused = save_profile(
                &store,
                &p,
                edit(&["dewow"], velocity),
                "a",
                "t",
                &Expectation::Any,
            );
            assert!(
                matches!(refused, Err(ProcessingError::Velocity(_))),
                "{velocity}"
            );
        }
        assert!(list_profiles(&store).unwrap().is_empty());
    }

    #[test]
    fn a_radargram_may_only_select_before_its_profile() {
        let (_dir, store) = store();
        let p = id("p");
        save_profile(
            &store,
            &p,
            edit(&["dewow"], 0.168),
            "a",
            "t",
            &Expectation::Any,
        )
        .unwrap();
        let line = RadargramId::new("line-01").unwrap();

        let assigned = assign(
            &store,
            &line,
            Some((
                p.clone(),
                vec![
                    "subset(0 2000)".to_string(),
                    "remove_traces(5-9)".to_string(),
                ],
            )),
        )
        .unwrap()
        .unwrap();
        assert!(assigned.pre_steps[0].starts_with("subset("));

        let refused = assign(&store, &line, Some((p.clone(), vec!["dewow".to_string()])));
        match refused {
            Err(ProcessingError::Steps(message)) => {
                assert!(message.contains("only choose"), "{message}")
            }
            other => panic!("{other:?}"),
        }
        // Refused, so the assignment before it stands.
        let (assignments, _) = read_assignments(&store).unwrap();
        assert_eq!(assignments.radargrams[&line], assigned);

        assert!(matches!(
            assign(&store, &line, Some((id("nope"), vec![]))),
            Err(ProcessingError::NoSuchProfile(_))
        ));
    }

    #[test]
    fn a_profile_in_use_cannot_be_deleted() {
        let (_dir, store) = store();
        let p = id("p");
        save_profile(
            &store,
            &p,
            edit(&["dewow"], 0.168),
            "a",
            "t",
            &Expectation::Any,
        )
        .unwrap();
        let line = RadargramId::new("line-01").unwrap();
        assign(&store, &line, Some((p.clone(), vec![]))).unwrap();
        assert_eq!(read_assignments(&store).unwrap().0.members(&p), [&line]);

        match delete_profile(&store, &p, &Expectation::Any) {
            Err(e @ ProcessingError::InUse { .. }) => {
                assert!(
                    e.to_string().contains("used by 1 radargram (line-01)"),
                    "{e}"
                )
            }
            other => panic!("{other:?}"),
        }
        assign(&store, &line, None).unwrap();
        assert!(delete_profile(&store, &p, &Expectation::Any).unwrap());
        assert!(read_profile(&store, &p).unwrap().is_none());
    }

    fn recipe_of(steps: &[String], velocity: f32) -> crate::recipe::Recipe {
        crate::recipe::Recipe {
            format: crate::recipe::RECIPE_FORMAT,
            core_version: crate::recipe::CORE_PROCESSING_VERSION,
            bundles: vec![bundle("aa")],
            dem: None,
            crs: "EPSG:32633".to_string(),
            medium_velocity: velocity,
            antenna_mhz_override: None,
            antenna_separation_override: None,
            steps: steps
                .iter()
                .map(|s| crate::recipe::RecipeStep {
                    step: s.clone(),
                    version: crate::steps::parse_one(s).unwrap().step.version(),
                })
                .collect(),
        }
    }

    fn bundle(data: &str) -> crate::recipe::Bundle {
        crate::recipe::Bundle {
            format: "ramac".to_string(),
            files: vec![crate::recipe::BundleFile {
                role: crate::recipe::FileRole::Data,
                file: crate::recipe::HashedFile {
                    name: "DAT_0001_A1.rd3".to_string(),
                    blake3: data.to_string(),
                },
            }],
        }
    }

    fn source(data: &str) -> super::super::raw::Source {
        super::super::raw::Source {
            bundles: vec![bundle(data)],
            crs: None,
            dem: None,
        }
    }

    #[test]
    fn a_radargram_is_current_until_its_profile_or_source_moves_on() {
        let (_dir, store) = store();
        let p = id("p");
        let (v1, _) = save_profile(
            &store,
            &p,
            edit(&["dewow"], 0.168),
            "a",
            "t",
            &Expectation::Any,
        )
        .unwrap();
        let assignment = Assignment {
            profile: p.clone(),
            pre_steps: canonical_pre_steps(&["subset(0 100)".to_string()]).unwrap(),
        };
        let processed = recipe_of(&expected_steps(&assignment, v1.current()), 0.168);
        let state = |profile: &Profile, source: &super::super::raw::Source| {
            state_of(&assignment, profile, Some(source), true, Some(&processed))
        };
        assert_eq!(
            state(&v1, &source("aa")),
            ProcessingState::Current { profile_version: 1 }
        );

        // The profile moves on: stale, and it says from which version.
        let (v2, _) = save_profile(
            &store,
            &p,
            edit(&["dewow", "gain(0.002)"], 0.168),
            "a",
            "t",
            &Expectation::Any,
        )
        .unwrap();
        match state(&v2, &source("aa")) {
            ProcessingState::Stale {
                profile_version: Some(1),
                reasons,
            } => {
                assert!(
                    reasons[0].contains("version 1 of the profile, which is now at version 2"),
                    "{reasons:?}"
                )
            }
            other => panic!("{other:?}"),
        }
        // The raw data is switched (a better .cor, say): stale.
        match state(&v1, &source("bb")) {
            ProcessingState::Stale { reasons, .. } => {
                assert!(reasons[0].contains("switched"), "{reasons:?}")
            }
            other => panic!("{other:?}"),
        }
        // A CRS the source now names that the file was not processed in.
        let mut crs = source("aa");
        crs.crs = Some("EPSG:32634".to_string());
        assert!(matches!(state(&v1, &crs), ProcessingState::Stale { .. }));
    }

    #[test]
    fn what_cannot_be_reprocessed_is_never_stale() {
        let (_dir, store) = store();
        let p = id("p");
        let (profile, _) = save_profile(
            &store,
            &p,
            edit(&["dewow"], 0.168),
            "a",
            "t",
            &Expectation::Any,
        )
        .unwrap();
        let assignment = Assignment {
            profile: p,
            pre_steps: vec![],
        };
        assert_eq!(
            state_of(&assignment, &profile, None, true, None),
            ProcessingState::NoRawData
        );
        assert_eq!(
            state_of(&assignment, &profile, Some(&source("aa")), false, None),
            ProcessingState::NotProcessed
        );
        // Served, but from before recipes: it cannot be compared, so it is
        // reprocessed to be sure.
        assert!(matches!(
            state_of(&assignment, &profile, Some(&source("aa")), true, None),
            ProcessingState::Stale {
                profile_version: None,
                ..
            }
        ));
    }

    #[test]
    fn an_older_step_is_older_processing_not_stale() {
        let (_dir, store) = store();
        let p = id("p");
        let (profile, _) = save_profile(
            &store,
            &p,
            edit(&["dewow"], 0.168),
            "a",
            "t",
            &Expectation::Any,
        )
        .unwrap();
        let assignment = Assignment {
            profile: p,
            pre_steps: vec![],
        };
        let mut processed = recipe_of(&profile.current().steps, 0.168);
        processed.steps[0].version = 0;
        match state_of(
            &assignment,
            &profile,
            Some(&source("aa")),
            true,
            Some(&processed),
        ) {
            ProcessingState::OlderProcessing {
                profile_version: 1,
                reasons,
            } => {
                assert!(reasons[0].starts_with("`dewow` has changed"), "{reasons:?}")
            }
            other => panic!("{other:?}"),
        }
        // Steps the profile never had: stale, with no version to name.
        let other = recipe_of(&canonical_steps(&["gain(0.1)".to_string()]).unwrap(), 0.168);
        assert!(matches!(
            state_of(
                &assignment,
                &profile,
                Some(&source("aa")),
                true,
                Some(&other)
            ),
            ProcessingState::Stale {
                profile_version: None,
                ..
            }
        ));
    }
}
