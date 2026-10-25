//! What a processed radargram was made from, recorded in the file (#382).
//!
//! A [`Recipe`] is everything that determines a processed file's contents:
//! the raw files, by checksum; the DEM, if any, and how it was sampled; the
//! CRS, medium velocity and antenna overrides; and the steps, each with its
//! version. Its hash is what
//! [`crate::identity::RevisionId::fingerprint_v2`] fingerprints, so the same
//! raw data processed the same way is the same revision, wherever and
//! whenever it was processed, and a different input or step is a different
//! one.
//!
//! What is deliberately *not* in it: file names and paths (moving or
//! renaming a recording does not change what it contains), the radargram
//! id, display name, group and user metadata (descriptions, not inputs),
//! and the Ridal release version (a release that changes only the GUI
//! changes nothing here). The release is still recorded in `ridal_version`.
//!
//! # Versions
//!
//! A change to what a step computes bumps that step's
//! [`crate::processing::steps::Step::version`]. A change to anything
//! outside the steps that reaches the file -- reading a format, merging, coordinates, the
//! travel-time and depth geometry, writing the NetCDF -- bumps
//! [`CORE_PROCESSING_VERSION`]. Either gives every recipe that involves it
//! a new hash. The golden test in `crate::processing::steps::digests`
//! fails when output changes without one.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::gpr::{LocationCorrection, GPR};

/// The version of everything outside the steps that determines a processed
/// file's contents. See the module documentation for when to bump it.
///
/// - 2: `antenna_frequency` written (#412).
/// - 3: UTM converted to WGS84 accurately (#480): `latitude` and
///   `longitude`, and where a DEM is sampled, moved by up to 4.6 m.
/// - 4: one `processing_log` line per step (#85): `auto_gain` no longer
///   adds a `gain` line of its own. The data are unchanged.
pub const CORE_PROCESSING_VERSION: u32 = 4;

/// The layout of [`Recipe`] as serialized into `ridal_recipe`.
pub const RECIPE_FORMAT: u32 = 1;

/// What a file is to the recording it belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "server", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum FileRole {
    Header,
    Data,
    Coordinates,
}

impl FileRole {
    fn tag(self) -> &'static str {
        match self {
            FileRole::Header => "header",
            FileRole::Data => "data",
            FileRole::Coordinates => "coordinates",
        }
    }
}

/// One file, by content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "server", derive(utoipa::ToSchema))]
pub struct HashedFile {
    /// The file name, for people reading the recipe. Not hashed.
    pub name: String,
    /// blake3 of the file's bytes, as 64 hex characters.
    pub blake3: String,
}

/// One file of a recording, by role and content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "server", derive(utoipa::ToSchema))]
pub struct BundleFile {
    pub role: FileRole,
    #[serde(flatten)]
    pub file: HashedFile,
}

/// The files of one raw recording, as they were read.
///
/// `coordinates` is the file the positions were actually read from: a
/// `--cor` override when one was given, otherwise the recording's own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "server", derive(utoipa::ToSchema))]
pub struct Bundle {
    /// The format's name, as `ridal formats` lists it (`ramac`, ...).
    pub format: String,
    pub files: Vec<BundleFile>,
}

/// The inputs of a processing run, as gathered when the raw files are
/// read. The rest of a [`Recipe`] is known only once processing is done.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Inputs {
    /// In the order they were merged.
    pub bundles: Vec<Bundle>,
    pub dem: Option<HashedFile>,
    pub antenna_mhz_override: Option<f32>,
    pub antenna_separation_override: Option<f32>,
}

/// One step as it ran: its canonical form, with defaults filled in, and the
/// version of the step that ran it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecipeStep {
    pub step: String,
    pub version: u32,
}

/// Everything that determines a processed file's contents.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Recipe {
    pub format: u32,
    pub core_version: u32,
    /// The raw recordings, in the order they were merged.
    pub bundles: Vec<Bundle>,
    pub dem: Option<HashedFile>,
    /// How `dem` was sampled, which depends on the GDAL that ran (#462).
    /// Left out where bilinear, both here and from the hash, so recipes
    /// from before it was recorded keep their hash; nearly all of them
    /// were bilinear, and a nearest one cannot be told apart afterwards.
    #[serde(default, skip_serializing_if = "crate::dem::Resampling::is_bilinear")]
    pub dem_resampling: crate::dem::Resampling,
    /// The CRS the coordinates were projected into, as the file records it.
    pub crs: String,
    /// m/ns.
    pub medium_velocity: f32,
    /// MHz.
    pub antenna_mhz_override: Option<f32>,
    /// m.
    pub antenna_separation_override: Option<f32>,
    pub steps: Vec<RecipeStep>,
}

impl Recipe {
    /// The recipe `gpr` was processed with (#382), or `None` when it was not
    /// read from raw files and its inputs are unknown.
    ///
    /// Each step's version is that of the step as registered now, which is
    /// the one that ran: [`GPR::steps`] holds only steps this process ran.
    pub fn of(gpr: &GPR) -> Result<Option<Recipe>, String> {
        let Some(inputs) = &gpr.inputs else {
            return Ok(None);
        };
        let steps = gpr
            .steps
            .iter()
            .map(|canonical| {
                let parsed = crate::processing::steps::parse_one(canonical)?;
                Ok(RecipeStep {
                    step: canonical.clone(),
                    version: parsed.step.version(),
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        Ok(Some(Recipe {
            format: RECIPE_FORMAT,
            core_version: CORE_PROCESSING_VERSION,
            bundles: inputs.bundles.clone(),
            dem: inputs.dem.clone(),
            dem_resampling: match &gpr.location.correction {
                LocationCorrection::Dem { resampling, .. } => *resampling,
                LocationCorrection::None | LocationCorrection::InCoordinates { .. } => {
                    crate::dem::Resampling::default()
                }
            },
            crs: gpr.location.crs.clone(),
            medium_velocity: gpr.metadata.medium_velocity,
            antenna_mhz_override: inputs.antenna_mhz_override,
            antenna_separation_override: inputs.antenna_separation_override,
            steps,
        }))
    }

    /// The recipe's fingerprint, as 64 hex characters.
    ///
    /// Every field is framed with its length, so no two different recipes
    /// can run together into the same bytes. File names are left out; see
    /// the module documentation for what else is, and why. The domain
    /// string changes if this framing ever does, so a recipe hashed one way
    /// can never collide with one hashed another.
    pub fn hash(&self) -> String {
        let mut hasher = blake3::Hasher::new();
        put(&mut hasher, b"ridal-recipe-v1");
        put(&mut hasher, &self.core_version.to_le_bytes());
        put(&mut hasher, &(self.bundles.len() as u64).to_le_bytes());
        for bundle in &self.bundles {
            put(&mut hasher, bundle.format.as_bytes());
            put(&mut hasher, &(bundle.files.len() as u64).to_le_bytes());
            for file in &bundle.files {
                put(&mut hasher, file.role.tag().as_bytes());
                put(&mut hasher, file.file.blake3.as_bytes());
            }
        }
        put_optional(&mut hasher, self.dem.as_ref().map(|d| d.blake3.as_bytes()));
        put(&mut hasher, self.crs.as_bytes());
        put(&mut hasher, &self.medium_velocity.to_le_bytes());
        put_optional(
            &mut hasher,
            self.antenna_mhz_override.map(f32::to_le_bytes).as_ref(),
        );
        put_optional(
            &mut hasher,
            self.antenna_separation_override
                .map(f32::to_le_bytes)
                .as_ref(),
        );
        put(&mut hasher, &(self.steps.len() as u64).to_le_bytes());
        for step in &self.steps {
            put(&mut hasher, step.step.as_bytes());
            put(&mut hasher, &step.version.to_le_bytes());
        }
        // Last, after the steps' count has fixed how many fields came
        // before, so one more cannot be read as any recipe without it.
        if !self.dem_resampling.is_bilinear() {
            put(&mut hasher, b"dem_resampling");
            put(&mut hasher, self.dem_resampling.as_str().as_bytes());
        }
        hasher.finalize().to_hex().to_string()
    }

    /// The recipe as `ridal_recipe` stores it.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("a recipe is plain data and always serializes")
    }
}

fn put(hasher: &mut blake3::Hasher, bytes: &[u8]) {
    hasher.update(&(bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

fn put_optional<T: AsRef<[u8]>>(hasher: &mut blake3::Hasher, value: Option<T>) {
    match value {
        None => {
            hasher.update(&[0]);
        }
        Some(bytes) => {
            hasher.update(&[1]);
            put(hasher, bytes.as_ref());
        }
    }
}

/// A file by its name and the blake3 of its bytes.
pub fn hash_file(path: &Path) -> Result<HashedFile, String> {
    let mut file = std::fs::File::open(path)
        .map_err(|e| format!("Could not read {path:?} to hash it: {e}"))?;
    let mut hasher = blake3::Hasher::new();
    hasher
        .update_reader(&mut file)
        .map_err(|e| format!("Could not read {path:?} to hash it: {e}"))?;
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default();
    Ok(HashedFile {
        name,
        blake3: hasher.finalize().to_hex().to_string(),
    })
}

/// The files of one recording, as `resolved` names them (with `--cor`
/// applied by [`crate::formats::with_coordinates`]).
///
/// A role whose file does not exist is left out rather than refused: the
/// readers decide what a recording needs, and one that loaded without it
/// did not need it. A data file that is the header itself (GSSI's `.DZT`)
/// is listed once.
pub fn bundle_of(resolved: &crate::formats::ResolvedInput) -> Result<Bundle, String> {
    let mut files = Vec::new();
    for (role, path) in [
        (FileRole::Header, resolved.header.as_path()),
        (FileRole::Data, resolved.data.as_path()),
        (FileRole::Coordinates, resolved.coordinates.as_path()),
    ] {
        if role == FileRole::Data && path == resolved.header {
            continue;
        }
        if path.is_file() {
            files.push(BundleFile {
                role,
                file: hash_file(path)?,
            });
        }
    }
    Ok(Bundle {
        format: crate::formats::format_info(resolved.kind).name.to_string(),
        files,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(role: FileRole, name: &str, blake3: &str) -> BundleFile {
        BundleFile {
            role,
            file: HashedFile {
                name: name.to_string(),
                blake3: blake3.to_string(),
            },
        }
    }

    fn recipe() -> Recipe {
        Recipe {
            format: RECIPE_FORMAT,
            core_version: 1,
            bundles: vec![Bundle {
                format: "ramac".to_string(),
                files: vec![
                    file(FileRole::Header, "DAT_0001_A1.rad", "aa"),
                    file(FileRole::Data, "DAT_0001_A1.rd3", "bb"),
                    file(FileRole::Coordinates, "DAT_0001_A1.cor", "cc"),
                ],
            }],
            dem: None,
            dem_resampling: crate::dem::Resampling::Bilinear,
            crs: "EPSG:32633".to_string(),
            medium_velocity: 0.168,
            antenna_mhz_override: None,
            antenna_separation_override: None,
            steps: vec![RecipeStep {
                step: "dewow(window=auto method=median)".to_string(),
                version: 1,
            }],
        }
    }

    #[test]
    fn the_hash_is_pinned() {
        // Every revision id written since #382 depends on this value. A
        // change to the framing changes every one of them, so it has to be
        // a new domain string (and a new revision scheme), never an edit.
        assert_eq!(
            recipe().hash(),
            "79b9a607c370e0ccc2a45be6267da3299a8ab067a5e7422055c18e9b5281d866"
        );
    }

    #[test]
    fn names_do_not_change_the_hash() {
        let mut renamed = recipe();
        renamed.bundles[0].files[1].file.name = "line-01.rd3".to_string();
        assert_eq!(recipe().hash(), renamed.hash());
    }

    /// One change to a recipe, and what it is called in a failure.
    type Change = (&'static str, fn(&mut Recipe));

    #[test]
    fn every_input_changes_the_hash() {
        let base = recipe().hash();
        let changes: Vec<Change> = vec![
            ("core version", |r| r.core_version = 2),
            ("a file's content", |r| {
                r.bundles[0].files[1].file.blake3 = "bd".to_string()
            }),
            ("a file's role", |r| {
                r.bundles[0].files[2].role = FileRole::Data
            }),
            ("the format", |r| r.bundles[0].format = "gssi".to_string()),
            ("a second bundle", |r| {
                let again = r.bundles[0].clone();
                r.bundles.push(again)
            }),
            ("a DEM", |r| {
                r.dem = Some(HashedFile {
                    name: "dem.tif".to_string(),
                    blake3: "dd".to_string(),
                })
            }),
            ("sampling the DEM at the nearest cell", |r| {
                r.dem_resampling = crate::dem::Resampling::Nearest
            }),
            ("the CRS", |r| r.crs = "EPSG:32634".to_string()),
            ("the velocity", |r| r.medium_velocity = 0.1),
            ("an antenna frequency override", |r| {
                r.antenna_mhz_override = Some(100.)
            }),
            ("an antenna separation override", |r| {
                r.antenna_separation_override = Some(2.)
            }),
            ("a step's version", |r| r.steps[0].version = 2),
            ("a step's arguments", |r| {
                r.steps[0].step = "dewow(window=10 method=median)".to_string()
            }),
            ("no steps", |r| r.steps.clear()),
        ];
        for (what, change) in changes {
            let mut changed = recipe();
            change(&mut changed);
            assert_ne!(changed.hash(), base, "{what} did not change the hash");
        }
    }

    #[test]
    fn the_order_of_merged_recordings_matters() {
        let mut two = recipe();
        let mut other = two.bundles[0].clone();
        other.files[1].file.blake3 = "ee".to_string();
        two.bundles.push(other);
        let mut swapped = two.clone();
        swapped.bundles.reverse();
        assert_ne!(two.hash(), swapped.hash());
    }

    #[test]
    fn an_override_does_not_collide_with_its_absence() {
        // `None` and `Some` are tagged, so no value of one field can be
        // mistaken for the next field's bytes.
        let mut a = recipe();
        a.antenna_mhz_override = Some(0.);
        let mut b = recipe();
        b.antenna_separation_override = Some(0.);
        assert_ne!(a.hash(), b.hash());
        assert_ne!(a.hash(), recipe().hash());
    }

    #[test]
    fn the_json_round_trips() {
        let mut recipe = recipe();
        let read: Recipe = serde_json::from_str(&recipe.to_json()).unwrap();
        assert_eq!(read, recipe);
        assert_eq!(read.hash(), recipe.hash());

        recipe.dem_resampling = crate::dem::Resampling::Nearest;
        assert!(recipe.to_json().contains(r#""dem_resampling":"nearest""#));
        let read: Recipe = serde_json::from_str(&recipe.to_json()).unwrap();
        assert_eq!(read, recipe);
    }

    #[test]
    fn a_bilinear_dem_is_written_as_before_462() {
        // Every recipe from before #462 that has a DEM reads as bilinear,
        // and one that is bilinear now writes and hashes as they did.
        let mut recipe = recipe();
        recipe.dem = Some(HashedFile {
            name: "dem.tif".to_string(),
            blake3: "dd".to_string(),
        });
        assert!(!recipe.to_json().contains("dem_resampling"));
        let mut before: serde_json::Value = serde_json::from_str(&recipe.to_json()).unwrap();
        before.as_object_mut().unwrap().remove("dem_resampling");
        let read: Recipe = serde_json::from_value(before).unwrap();
        assert_eq!(read.dem_resampling, crate::dem::Resampling::Bilinear);
        assert_eq!(read.hash(), recipe.hash());
    }

    #[test]
    fn a_ramac_bundle_lists_its_three_files_and_takes_the_override() {
        let rad = Path::new("assets/mala/dronbreen-20250327-DAT_0066_A1.rad");
        let resolved = crate::formats::resolve_input(rad).unwrap();
        let bundle = bundle_of(&resolved).unwrap();
        assert_eq!(bundle.format, "ramac");
        let roles: Vec<FileRole> = bundle.files.iter().map(|f| f.role).collect();
        assert_eq!(
            roles,
            vec![FileRole::Header, FileRole::Data, FileRole::Coordinates]
        );
        assert_eq!(
            bundle.files[0].file.name,
            "dronbreen-20250327-DAT_0066_A1.rad"
        );
        assert_eq!(bundle.files[0].file.blake3.len(), 64);

        let other_cor = Path::new("assets/mala/dronbreen-20220329-DAT_0237_A1.cor");
        let mut with_other = [resolved.clone()];
        crate::formats::with_coordinates(&mut with_other, Some(other_cor)).unwrap();
        let overridden = bundle_of(&with_other[0]).unwrap();
        assert_eq!(overridden.files[..2], bundle.files[..2]);
        assert_eq!(overridden.files[2].file, hash_file(other_cor).unwrap());
        assert_ne!(overridden.files[2], bundle.files[2]);
    }
}
