//! A project's own render profiles (#183).
//!
//! The built-in profiles ([`RenderProfile::built_in_profiles`]) are the
//! server's; these are the project's, made and changed by its operators and
//! offered next to the built-ins everywhere a profile is chosen. Each is a
//! whole [`RenderProfile`], not a set of overrides on a built-in, so retuning
//! a built-in in a later release never changes a project's picture.
//!
//! ```text
//! ridal_data/render/profiles/<name>.json   one profile
//! ```
//!
//! Edits are in place, with no history: a render is re-made from the profile
//! whenever it is asked for, so there is nothing an older version would let
//! anyone reproduce that the current one does not. Every render's cache key
//! is built from the profile's content (`RenderProfile::cache_key_fragment`),
//! so an edit is never served from a cache of the previous version.
//!
//! The rules on names keep the three scopes apart: a project profile cannot
//! take a built-in's name, nor start with
//! [`PERSONAL_PROFILE_PREFIX`], which belongs to personal profiles.

// The server is the only writer and reader; a CLI-only build has neither.
#![cfg_attr(not(feature = "server"), allow(dead_code))]

use std::path::{Path, PathBuf};

use super::store::{DocumentStore, Expectation, StoreError, Version};
use crate::identity::RenderProfileName;
use crate::render::profile::{RenderProfile, PERSONAL_PROFILE_PREFIX};

const PROFILES: &str = "render/profiles";

#[derive(Debug)]
pub enum RenderProfileError {
    Store(StoreError),
    /// A document on disk that does not parse, or parses to a profile that
    /// is not valid.
    Malformed {
        path: PathBuf,
        message: String,
    },
    /// A name that is not a valid slug.
    InvalidName(String),
    /// A name a built-in profile has.
    BuiltIn(String),
    /// A name in the personal profiles' namespace.
    Reserved(String),
    /// The body names a different profile than the path.
    NameMismatch {
        path: String,
        body: String,
    },
    /// A profile [`RenderProfile::validate`] refuses.
    Invalid(String),
}

impl std::fmt::Display for RenderProfileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RenderProfileError::Store(e) => write!(f, "{e}"),
            RenderProfileError::Malformed { path, message } => {
                write!(f, "{} cannot be used: {message}", path.display())
            }
            RenderProfileError::InvalidName(message) => write!(f, "{message}"),
            RenderProfileError::BuiltIn(name) => write!(
                f,
                "'{name}' is a built-in render profile; give the project's profile another name"
            ),
            RenderProfileError::Reserved(name) => write!(
                f,
                "'{name}' starts with '{PERSONAL_PROFILE_PREFIX}', which only personal \
                 render profiles may"
            ),
            RenderProfileError::NameMismatch { path, body } => write!(
                f,
                "the profile is called '{body}' but was sent to '{path}'; the two must agree"
            ),
            RenderProfileError::Invalid(message) => {
                write!(f, "the render profile cannot be used: {message}")
            }
        }
    }
}

impl From<StoreError> for RenderProfileError {
    fn from(e: StoreError) -> Self {
        RenderProfileError::Store(e)
    }
}

/// `name` as a project profile's name: a slug, not a built-in's name, and
/// not a personal one's.
pub fn check_name(name: &str) -> Result<RenderProfileName, RenderProfileError> {
    let parsed = RenderProfileName::new(name).map_err(RenderProfileError::InvalidName)?;
    if RenderProfile::by_name(name).is_some() {
        return Err(RenderProfileError::BuiltIn(name.to_string()));
    }
    if name.starts_with(PERSONAL_PROFILE_PREFIX) {
        return Err(RenderProfileError::Reserved(name.to_string()));
    }
    Ok(parsed)
}

fn profile_path(name: &RenderProfileName) -> PathBuf {
    Path::new(PROFILES).join(format!("{}.json", name.as_str()))
}

/// One profile and its version, or `None` when the project has no profile
/// of that name.
///
/// A file whose profile is named differently from the file, or which
/// [`RenderProfile::validate`] refuses, is [`RenderProfileError::Malformed`]:
/// it was edited by hand, and rendering it would either answer to the wrong
/// name or fail in the render path.
pub fn read(
    store: &DocumentStore,
    name: &RenderProfileName,
) -> Result<Option<(RenderProfile, Version)>, RenderProfileError> {
    let path = profile_path(name);
    let Some(document) = store.read(&path)? else {
        return Ok(None);
    };
    let malformed = |message: String| RenderProfileError::Malformed {
        path: path.clone(),
        message,
    };
    let profile: RenderProfile =
        serde_json::from_str(&document.text).map_err(|e| malformed(e.to_string()))?;
    if profile.name != name.as_str() {
        return Err(malformed(format!(
            "it holds a profile called '{}'",
            profile.name
        )));
    }
    profile.validate().map_err(malformed)?;
    Ok(Some((profile, document.version)))
}

/// Every profile the project has, by name, and what kept any other file in
/// the directory from being one.
///
/// Lenient, because the list is what every page offers: one hand-broken
/// file must not take the others, or the viewer, down with it. A profile
/// whose name a built-in has taken since it was made (a later release
/// adding one) is skipped too, since the built-in answers to that name.
pub fn list(
    store: &DocumentStore,
) -> Result<(Vec<RenderProfile>, Vec<RenderProfileError>), RenderProfileError> {
    let mut profiles = Vec::new();
    let mut problems = Vec::new();
    for stem in store.list_stems(Path::new(PROFILES), ".json")? {
        let name = match check_name(&stem) {
            Ok(name) => name,
            Err(e) => {
                problems.push(RenderProfileError::Malformed {
                    path: Path::new(PROFILES).join(format!("{stem}.json")),
                    message: e.to_string(),
                });
                continue;
            }
        };
        match read(store, &name) {
            Ok(Some((profile, _))) => profiles.push(profile),
            Ok(None) => {}
            Err(e) => problems.push(e),
        }
    }
    Ok((profiles, problems))
}

/// Create or replace the profile called `name`.
///
/// `profile.name` must be `name`: a body naming another profile is more
/// likely a mistake than a rename, and a rename is a new profile plus a
/// delete, which keeps the references to the old name visibly broken
/// rather than silently pointing somewhere else.
pub fn save(
    store: &DocumentStore,
    name: &str,
    profile: RenderProfile,
    expected: &Expectation,
) -> Result<(RenderProfile, Version), RenderProfileError> {
    let name = check_name(name)?;
    if profile.name != name.as_str() {
        return Err(RenderProfileError::NameMismatch {
            path: name.to_string(),
            body: profile.name,
        });
    }
    profile.validate().map_err(RenderProfileError::Invalid)?;
    let text = serde_json::to_string_pretty(&profile).expect("a render profile is plain data");
    let version = store.write(&profile_path(&name), &text, expected)?;
    Ok((profile, version))
}

/// Delete the profile called `name`; `false` when there was none.
///
/// Whether it may be deleted (it is not the project's default) is the
/// caller's to check, since the default lives in `ridal.toml`.
pub fn delete(
    store: &DocumentStore,
    name: &RenderProfileName,
    expected: &Expectation,
) -> Result<bool, RenderProfileError> {
    Ok(store.remove(&profile_path(name), expected)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, DocumentStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = DocumentStore::new(dir.path().to_path_buf());
        (dir, store)
    }

    fn named(name: &str) -> RenderProfile {
        RenderProfile {
            name: name.to_string(),
            contrast: 1.5,
            nodata_color: Some([255, 0, 255]),
            ..RenderProfile::seismic_profile()
        }
    }

    #[test]
    fn a_saved_profile_reads_back_and_lists() {
        let (_dir, store) = store();
        let (saved, version) =
            save(&store, "survey", named("survey"), &Expectation::Absent).unwrap();
        let (read_back, read_version) = read(&store, &check_name("survey").unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(read_back, saved);
        assert_eq!(read_version, version);
        let (profiles, problems) = list(&store).unwrap();
        assert_eq!(profiles, vec![saved]);
        assert!(problems.is_empty());
    }

    #[test]
    fn writes_are_conditional() {
        let (_dir, store) = store();
        let (_, version) = save(&store, "survey", named("survey"), &Expectation::Absent).unwrap();
        // Creating it again is refused, as is an edit against a stale
        // version; the current version is accepted.
        assert!(matches!(
            save(&store, "survey", named("survey"), &Expectation::Absent),
            Err(RenderProfileError::Store(StoreError::Conflict { .. }))
        ));
        let (_, newer) = save(
            &store,
            "survey",
            RenderProfile {
                contrast: 2.0,
                ..named("survey")
            },
            &Expectation::Version(version.clone()),
        )
        .unwrap();
        assert!(matches!(
            save(
                &store,
                "survey",
                named("survey"),
                &Expectation::Version(version)
            ),
            Err(RenderProfileError::Store(StoreError::Conflict { .. }))
        ));
        let name = check_name("survey").unwrap();
        assert!(delete(&store, &name, &Expectation::Version(newer)).unwrap());
        assert!(!delete(&store, &name, &Expectation::Any).unwrap());
    }

    #[test]
    fn names_stay_out_of_the_built_in_and_personal_namespaces() {
        let (_dir, store) = store();
        for (name, expect) in [
            ("seismic", "built-in"),
            ("personal-mine", "personal"),
            ("Has Spaces", "disallowed character"),
        ] {
            let error = save(&store, name, named(name), &Expectation::Any).unwrap_err();
            assert!(error.to_string().contains(expect), "{name}: {error}");
        }
        // No built-in may ever take the personal prefix either, or a
        // personal profile could shadow it.
        assert!(RenderProfile::built_in_profiles()
            .iter()
            .all(|p| !p.name.starts_with(PERSONAL_PROFILE_PREFIX)));
    }

    #[test]
    fn the_body_must_name_the_profile_it_is_saved_as() {
        let (_dir, store) = store();
        let error = save(&store, "survey", named("other"), &Expectation::Any).unwrap_err();
        assert!(
            matches!(error, RenderProfileError::NameMismatch { .. }),
            "{error}"
        );
    }

    #[test]
    fn an_invalid_profile_is_refused() {
        let (_dir, store) = store();
        let profile = RenderProfile {
            contrast: f32::NAN,
            ..named("survey")
        };
        let error = save(&store, "survey", profile, &Expectation::Any).unwrap_err();
        assert!(matches!(error, RenderProfileError::Invalid(_)), "{error}");
    }

    #[test]
    fn one_broken_file_does_not_hide_the_others() {
        let (dir, store) = store();
        save(&store, "good", named("good"), &Expectation::Absent).unwrap();
        let profiles_dir = dir.path().join(PROFILES);
        // Hand edits: unparsable, renamed inside, out of range, and a
        // name a built-in has.
        std::fs::write(profiles_dir.join("broken.json"), "{").unwrap();
        let renamed = serde_json::to_string(&named("elsewhere")).unwrap();
        std::fs::write(profiles_dir.join("renamed.json"), renamed).unwrap();
        let mut wild = named("wild");
        wild.contrast = 1e9;
        std::fs::write(
            profiles_dir.join("wild.json"),
            serde_json::to_string(&wild).unwrap(),
        )
        .unwrap();
        std::fs::write(
            profiles_dir.join("seismic.json"),
            serde_json::to_string(&RenderProfile::seismic_profile()).unwrap(),
        )
        .unwrap();

        let (profiles, problems) = list(&store).unwrap();
        assert_eq!(
            profiles.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
            vec!["good"]
        );
        assert_eq!(problems.len(), 4, "{problems:?}");
        assert!(problems
            .iter()
            .all(|p| matches!(p, RenderProfileError::Malformed { .. })));
    }
}
