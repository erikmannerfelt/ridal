/// Tools to read elevation from Digital Elevation Models (DEMs)
use std::path::Path;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::coords::Coord;

/// How a DEM is read between its cell centres.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Resampling {
    /// Interpolated from the four nearest cells (`-r bilinear`).
    #[default]
    Bilinear,
    /// The value of the cell a point falls in: all `gdallocationinfo`
    /// can do before GDAL 3.10. Along a track this gives steps of up to a
    /// cell's relief rather than a smooth surface.
    Nearest,
}

impl Resampling {
    pub fn as_str(self) -> &'static str {
        match self {
            Resampling::Bilinear => "bilinear",
            Resampling::Nearest => "nearest",
        }
    }

    pub fn is_bilinear(&self) -> bool {
        *self == Resampling::Bilinear
    }
}

/// The first GDAL release whose `gdallocationinfo` takes `-r` (#462).
pub const BILINEAR_SINCE: (u32, u32) = (3, 10);

/// The `major.minor.patch` of a `--version` line such as
/// `GDAL 3.7.3, released 2023/10/30` or `GDAL 3.13.3 "Iowa City", ...`.
fn parse_gdal_version(line: &str) -> Option<(u32, u32, u32)> {
    let version = line.trim().strip_prefix("GDAL ")?;
    let mut parts = version.split('.').map(|part| {
        let digits: String = part.chars().take_while(char::is_ascii_digit).collect();
        digits.parse::<u32>().ok()
    });
    let major = parts.next()??;
    let minor = parts.next()??;
    let patch = parts.next().flatten().unwrap_or(0);
    Some((major, minor, patch))
}

/// The version of the GDAL that `gdallocationinfo` belongs to.
///
/// Asked once per process: it decides how every DEM is sampled, and a
/// change of GDAL under a running server needs a restart anyway. Only a
/// version that was read is kept, so a failure is reported each time.
pub fn gdal_version() -> Result<(u32, u32, u32), String> {
    static VERSION: OnceLock<(u32, u32, u32)> = OnceLock::new();
    if let Some(version) = VERSION.get() {
        return Ok(*version);
    }
    let output = crate::tools::spawn_tool(
        std::process::Command::new("gdallocationinfo")
            .arg("--version")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped()),
    )
    .map_err(|e| spawn_error("gdallocationinfo", &e))?
    .wait_with_output()
    .map_err(|e| format!("Call process error: {e}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let version = parse_gdal_version(&stdout).ok_or_else(|| {
        format!(
            "Could not read GDAL's version from `gdallocationinfo --version`: {:?}",
            stdout.trim()
        )
    })?;
    Ok(*VERSION.get_or_init(|| version))
}

/// How this machine's GDAL samples a DEM.
pub fn resampling() -> Result<Resampling, String> {
    let (major, minor, _) = gdal_version()?;
    Ok(if (major, minor) >= BILINEAR_SINCE {
        Resampling::Bilinear
    } else {
        Resampling::Nearest
    })
}

/// What the person running a server should know about how it samples
/// DEMs, or `None` when GDAL samples them bilinearly (#462).
///
/// Worked out once per process, failure included, so a page can show it
/// without starting a tool on every view: the servers ask it before they
/// listen, which is also where they print it.
pub fn resampling_warning() -> Option<&'static str> {
    static WARNING: OnceLock<Option<String>> = OnceLock::new();
    WARNING
        .get_or_init(|| match gdal_version() {
            Ok((major, minor, _)) if (major, minor) >= BILINEAR_SINCE => None,
            Ok((major, minor, patch)) => Some(format!(
                "GDAL {major}.{minor}.{patch} samples a DEM at the nearest cell only, so \
                 elevations along a radargram step from one cell to the next. Install \
                 GDAL {}.{} or later and restart Ridal to sample DEMs bilinearly; \
                 radargrams processed with a DEM until then are listed as older \
                 processing.",
                BILINEAR_SINCE.0, BILINEAR_SINCE.1
            )),
            Err(e) => Some(format!("No DEM can be sampled: {e}")),
        })
        .as_deref()
}

/// What [`sample_dem`] read, and how.
#[derive(Debug, Clone, PartialEq)]
pub struct DemSample {
    /// One per coordinate, in the DEM's units (m for every DEM Ridal is
    /// used with).
    pub elevations: Vec<f32>,
    pub resampling: Resampling,
}

/// Describe a failure to launch one of GDAL's command-line tools.
///
/// GDAL is an external dependency, so "not installed" is an ordinary thing
/// for a user to hit rather than a bug. The bare io error is
/// `No such file or directory (os error 2)`, which does not say *which*
/// file -- easily read as the DEM being missing when it is GDAL itself.
///
/// Matches on [`std::io::ErrorKind`] rather than the message text, which is
/// platform- and locale-dependent.
fn spawn_error(program: &str, error: &std::io::Error) -> String {
    if error.kind() == std::io::ErrorKind::NotFound {
        format!("GDAL ({program}) cannot be found / is not installed: {error}")
    } else {
        format!("Call error when spawning process: {error}")
    }
}

fn run_gdallocationinfo(
    dem_path: &Path,
    coords_wgs84: &[Coord],
    resampling: Resampling,
) -> Result<std::process::Output, String> {
    let dem_str = dem_path.to_str().ok_or("Empty DEM path given")?;

    let mut args = vec!["-xml", "-b", "1", "-wgs84", dem_str];

    if resampling == Resampling::Bilinear {
        args.push("-r");
        args.push("bilinear");
    }

    let child = crate::tools::spawn_tool(
        std::process::Command::new("gdallocationinfo")
            .args(&args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped()),
    )
    .map_err(|e| spawn_error("gdallocationinfo", &e))?;

    let mut buf = String::new();
    for coord in coords_wgs84 {
        buf.push_str(&format!("{} {}\n", coord.x, coord.y));
    }

    crate::tools::communicate(child, buf.into_bytes())
        .map_err(|e| format!("Call process error: {e}"))
}

fn parse_elevations_from_output(
    output: &std::process::Output,
    coords_wgs84: &[Coord],
) -> Result<Vec<f32>, String> {
    let parsed = String::from_utf8_lossy(&output.stdout);
    let mut elevations = Vec::<f32>::new();

    for line in parsed.lines().map(|s| s.trim()) {
        if line.contains("<Value>") {
            elevations.push(
                line.replace("<Value>", "")
                    .replace("</Value>", "")
                    .parse()
                    .map_err(|e| format!("Error parsing <Value>: {e}"))?,
            );
        } else if line.contains("<Alert>") {
            let error = line.replace("<Alert>", "").replace("</Alert>", "");
            let coord = coords_wgs84[elevations.len()];
            return Err(format!(
                "Error parsing coord (lon: {:.3}, lat: {:.3}): {}",
                coord.x, coord.y, error
            ));
        }
    }

    if elevations.len() != coords_wgs84.len() {
        let stderr_str = String::from_utf8_lossy(&output.stderr);

        if !stderr_str.is_empty() {
            return Err(format!("DEM sampling error: {}", stderr_str));
        }

        // Empty output but no stderr - gdallocationinfo may have failed silently
        if elevations.is_empty() {
            return Err("DEM sampling failed. gdallocationinfo returned no data. Check that GDAL is properly installed and in PATH.".to_string());
        }

        return Err(format!(
            "Shape error. Length of sampled elevations ({}) does not align with length of coordinates ({})",
            elevations.len(),
            coords_wgs84.len()
        ));
    }

    Ok(elevations)
}

/// Sample `dem_path` at each of `coords_wgs84` (lon, lat).
///
/// Bilinearly where GDAL can, at the nearest cell before GDAL 3.10 (see
/// [`resampling`]), which the result says so that it can be recorded.
pub fn sample_dem(dem_path: &Path, coords_wgs84: &[Coord]) -> Result<DemSample, String> {
    if coords_wgs84.is_empty() {
        return Err("Coords vec is empty.".into());
    }

    let resampling = resampling()?;
    if resampling == Resampling::Nearest {
        let (major, minor, patch) = gdal_version()?;
        eprintln!(
            "Warning: GDAL {major}.{minor}.{patch} samples a DEM at the nearest cell only; \
             GDAL {}.{} or later is needed to interpolate bilinearly.",
            BILINEAR_SINCE.0, BILINEAR_SINCE.1
        );
    }
    let output = run_gdallocationinfo(dem_path, coords_wgs84, resampling)?;
    Ok(DemSample {
        elevations: parse_elevations_from_output(&output, coords_wgs84)?,
        resampling,
    })
}

/// Check that GDAL reads `dem_path` as a raster, so a DEM that is not one
/// is refused when it is added rather than when the first radargram is
/// processed with it.
pub fn check_readable(dem_path: &Path) -> Result<(), String> {
    let output = crate::tools::spawn_tool(
        std::process::Command::new("gdalinfo")
            .args(["-nomd", "-norat", "-noct"])
            .arg(dem_path)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped()),
    )
    .map_err(|e| spawn_error("gdalinfo", &e))?
    .wait_with_output()
    .map_err(|e| format!("Call process error: {e}"))?;
    if output.status.success() {
        Ok(())
    } else {
        // GDAL's first line, from after the path: the path is a stored
        // hash here, and means nothing to whoever uploaded the file.
        let stderr = String::from_utf8_lossy(&output.stderr);
        let path = dem_path.to_string_lossy();
        let reason = stderr
            .lines()
            .next()
            .and_then(|line| line.split_once(&*path))
            .map(|(_, after)| after.trim_start_matches(['\'', '`', ' ']).trim())
            .filter(|reason| !reason.is_empty());
        Err(match reason {
            Some(reason) => format!("it is not a raster GDAL can read ({reason})"),
            None => "it is not a raster GDAL can read".to_string(),
        })
    }
}

#[cfg(test)]
mod tests {

    use std::path::{Path, PathBuf};

    #[test]
    fn a_raster_is_readable_and_anything_else_is_not() {
        super::check_readable(&get_dem_path()).unwrap();
        let refused = super::check_readable(Path::new("assets/mala/README.md")).unwrap_err();
        assert!(refused.contains("not a raster"), "{refused}");
        assert!(!refused.contains("README"), "{refused}");
    }

    use crate::coords::{Coord, Crs, UtmCrs};

    /// The PATH-manipulating test below can only run on some platforms and
    /// skips itself if GDAL is still reachable. This pins the same rule
    /// directly, everywhere.
    #[test]
    fn a_missing_gdal_is_reported_by_program_name() {
        let missing = std::io::Error::from(std::io::ErrorKind::NotFound);
        let message = super::spawn_error("gdallocationinfo", &missing);
        assert!(
            message.contains("GDAL (gdallocationinfo) cannot be found / is not installed"),
            "{message}"
        );

        // Anything else is not a missing install, and saying so would send
        // the reader off installing software they already have.
        let denied = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        let message = super::spawn_error("gdallocationinfo", &denied);
        assert!(!message.contains("is not installed"), "{message}");
        assert!(
            message.contains("Call error when spawning process"),
            "{message}"
        );
    }

    fn get_dem_path() -> PathBuf {
        Path::new("assets/test_dem_dtm20_mettebreen.tif").to_path_buf()
    }

    fn make_test_coords() -> Vec<(Coord, Result<f32, String>)> {
        vec![
            (
                Coord {
                    x: 553802.,
                    y: 8639550.,
                },
                // GDAL's bilinear value at this point (#480); before, the
                // conversion to WGS84 sampled 4.4 m north of it (422.0352).
                Ok(421.68707_f32),
            ),
            (
                Coord {
                    x: 553820.,
                    y: 8639550.,
                },
                Ok(422.8686_f32),
            ),
            (
                Coord { x: 0., y: 8639550. },
                Err("Location is off this file".to_string()),
            ),
        ]
    }

    #[test]
    fn a_gdal_version_is_read_from_any_release_line() {
        use super::parse_gdal_version as parse;
        assert_eq!(parse("GDAL 3.7.3, released 2023/10/30\n"), Some((3, 7, 3)));
        assert_eq!(
            parse("GDAL 3.13.3 \"Iowa City\", released 2026/08/13"),
            Some((3, 13, 3))
        );
        assert_eq!(parse("GDAL 3.10.0dev-0d5ae0d4, released"), Some((3, 10, 0)));
        assert_eq!(parse("GDAL 3.10, released"), Some((3, 10, 0)));
        assert_eq!(parse("Usage: gdallocationinfo"), None);
        assert_eq!(parse(""), None);
        // 3.10 is above 3.9, which a comparison of strings gets wrong.
        assert!((3, 10) > (3, 9));
    }

    #[test]
    fn the_resampling_follows_the_installed_gdal() {
        let (major, minor, _) = super::gdal_version().unwrap();
        let expected = if (major, minor) >= super::BILINEAR_SINCE {
            super::Resampling::Bilinear
        } else {
            super::Resampling::Nearest
        };
        assert_eq!(super::resampling().unwrap(), expected);
    }

    #[test]
    #[cfg(not(target_os = "windows"))] // Added 2026-04-12 because gdal stopped working properly in CI
    fn test_read_elevations() {
        let coords_elevs = make_test_coords();
        let working_coords = coords_elevs
            .iter()
            .filter(|(_c, e)| e.is_ok())
            .map(|(c, _e)| *c)
            .collect::<Vec<Coord>>();
        let all_coords = coords_elevs
            .iter()
            .map(|(c, _e)| *c)
            .collect::<Vec<Coord>>();
        let crs = Crs::Utm(UtmCrs {
            zone: 33,
            north: true,
        });

        let dem_path = get_dem_path();

        let supports_interpolation = super::resampling().unwrap() == super::Resampling::Bilinear;

        println!("Sampling DEM");
        let coords_wgs84 = crate::coords::to_wgs84(&working_coords, &crs).unwrap();
        super::sample_dem(&dem_path, &coords_wgs84).unwrap();

        let coords_wgs84 = crate::coords::to_wgs84(&all_coords, &crs).unwrap();
        super::sample_dem(&dem_path, &coords_wgs84).err().unwrap();

        for (coord, expected) in coords_elevs {
            let coord_wgs84 = crate::coords::to_wgs84(&[coord], &crs).unwrap();

            let result = super::sample_dem(&dem_path, &coord_wgs84).map(|s| s.elevations);

            // The tests validate on bilinearly interpolated coordinates. This will fail if it's nearest
            if supports_interpolation {
                if let Ok(expected_elevation) = expected {
                    assert_eq!(Ok(vec![expected_elevation]), result);
                } else if let Err(expected_err_str) = expected {
                    if let Err(err_str) = result {
                        assert!(
                            err_str.contains(&expected_err_str),
                            "{} != {}",
                            err_str,
                            expected_err_str
                        );
                    } else {
                        panic!("Should have been an error but wasn't: {result:?}");
                    }
                }
            }
        }
        let wrong_path = dem_path.with_extension("tiffffff");
        assert!(super::sample_dem(&wrong_path, &coords_wgs84)
            .err()
            .unwrap()
            .contains("No such file or directory"));
    }

    #[test]
    #[cfg(not(target_os = "windows"))]
    fn a_dem_gdal_cannot_open_is_named_for_a_long_profile() {
        // `gdallocationinfo` exits without reading its input when it cannot
        // open the DEM, as an older one also did on `-r bilinear` (GDAL <
        // 3.10). With more coordinates than a pipe holds, as any real
        // profile has, that surfaced as "Broken pipe (os error 32)" (#461).
        let crs = Crs::Utm(UtmCrs {
            zone: 33,
            north: true,
        });
        let coord = make_test_coords()[0].0;
        let coords_wgs84 = crate::coords::to_wgs84(&vec![coord; 10_000], &crs).unwrap();
        let wrong_path = get_dem_path().with_extension("tiffffff");

        let err = super::sample_dem(&wrong_path, &coords_wgs84).unwrap_err();
        assert!(err.contains("No such file or directory"), "{err}");
    }

    #[test]
    #[cfg(not(target_os = "windows"))]
    fn test_no_gdal_failure() {
        let crs = Crs::Utm(UtmCrs {
            zone: 33,
            north: true,
        });

        let working_coords: Vec<Coord> = make_test_coords().iter().map(|(c, _)| *c).collect();

        let dem_path = get_dem_path();

        let coords_wgs84 = crate::coords::to_wgs84(&working_coords, &crs).unwrap();

        crate::tools::without_tools(|| {
            let res = super::sample_dem(&dem_path, &coords_wgs84);
            // Names the program Ridal actually tried to run. `sample_dem`
            // spawns `gdallocationinfo`, not `gdalinfo`, and saying so is
            // the whole point: "No such file or directory" alone reads as
            // the DEM being missing.
            assert!(
                res.as_ref()
                    .err()
                    .unwrap()
                    .contains("GDAL (gdallocationinfo) cannot be found / is not installed"),
                "Error {:?} should name the missing GDAL program",
                res
            );
        });
    }
}
