// src/export.rs

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use ndarray::Array2;
#[cfg(feature = "python")]
use numpy::{PyArray1, PyArray2};
#[cfg(feature = "python")]
use pyo3::{prelude::*, types::PyDict};

use crate::gpr::{LocationCorrection, GPR};
use crate::user_metadata;

/// Attribute values to support in the exported Dataset.
#[derive(Clone, Debug)]
pub enum ExportAttr {
    String(String),
    Strings(Vec<String>),
    F64(f64),
    F32(f32),
    I64(i64),
    U8(u8),
}

#[cfg(feature = "python")]
impl ExportAttr {
    pub fn to_python<'py>(&self, py: Python<'py>) -> PyResult<PyObject> {
        match self {
            ExportAttr::String(s) => Ok(s.into_py(py)),
            ExportAttr::Strings(vs) => Ok(vs.clone().into_py(py)),
            ExportAttr::F64(x) => {
                let item = PyArray1::from_slice_bound(py, &[*x]).get_item(0)?;
                Ok(item.into_py(py))
            }
            ExportAttr::F32(x) => {
                let item = PyArray1::from_slice_bound(py, &[*x]).get_item(0)?;
                Ok(item.into_py(py))
            }
            ExportAttr::I64(x) => {
                let item = PyArray1::from_slice_bound(py, &[*x]).get_item(0)?;
                Ok(item.into_py(py))
            }
            ExportAttr::U8(x) => {
                let item = PyArray1::from_slice_bound(py, &[*x]).get_item(0)?;
                Ok(item.into_py(py))
            }
        }
    }
}
impl From<&str> for ExportAttr {
    fn from(value: &str) -> Self {
        ExportAttr::String(value.to_string())
    }
}

impl From<f64> for ExportAttr {
    fn from(value: f64) -> Self {
        ExportAttr::F64(value)
    }
}

impl From<String> for ExportAttr {
    fn from(value: String) -> Self {
        ExportAttr::String(value)
    }
}

#[derive(Clone, Debug)]
pub enum ExportArray<'a> {
    F32Borrowed2D(&'a Array2<f32>),
    F32Owned1D(Vec<f32>),
    F64Owned1D(Vec<f64>),
    U32Owned1D(Vec<u32>),
    U8Scalar(u8), // For grid_mapping
}

#[derive(Clone, Debug)]
pub struct ExportVariable<'a> {
    pub dims: Vec<String>,
    pub data: ExportArray<'a>,
    /// Variable attributes (e.g., "unit", "coordinates")
    pub attrs: BTreeMap<String, ExportAttr>,
}

#[derive(Clone, Debug)]
pub struct ExportDataset<'a> {
    /// Dimensions (name -> length)
    pub dims: BTreeMap<String, usize>,
    /// Coordinate variables
    pub coords: BTreeMap<String, ExportVariable<'a>>,
    /// Data variables
    pub data_vars: BTreeMap<String, ExportVariable<'a>>,
    /// Global attributes (typed)
    pub attrs: BTreeMap<String, ExportAttr>,
}

#[cfg(feature = "python")]
impl<'a> ExportDataset<'a> {
    /// Convert to a Python dict that matches `xarray.Dataset.from_dict(...)`.
    #[allow(dead_code)]
    pub fn to_python<'py>(&self, py: Python<'py>) -> PyResult<PyObject> {
        let out = PyDict::new_bound(py);

        // dims
        let dims_py = PyDict::new_bound(py);
        for (k, v) in &self.dims {
            dims_py.set_item(k, *v)?;
        }
        out.set_item("dims", dims_py)?;

        // attrs
        let attrs_py = PyDict::new_bound(py);
        for (k, v) in &self.attrs {
            attrs_py.set_item(k, v.to_python(py)?)?;
        }
        out.set_item("attrs", attrs_py)?;

        // coords
        let coords_py = PyDict::new_bound(py);
        for (name, var) in &self.coords {
            coords_py.set_item(name, export_var_to_py(py, var)?)?;
        }
        out.set_item("coords", coords_py)?;

        // data_vars
        let dvs_py = PyDict::new_bound(py);
        for (name, var) in &self.data_vars {
            dvs_py.set_item(name, export_var_to_py(py, var)?)?;
        }
        out.set_item("data_vars", dvs_py)?;

        Ok(out.into())
    }
}

#[cfg(feature = "python")]
#[allow(dead_code)]
fn export_var_to_py<'py>(py: Python<'py>, var: &ExportVariable<'_>) -> PyResult<PyObject> {
    let dict = PyDict::new_bound(py);

    dict.set_item("dims", var.dims.clone())?;

    match &var.data {
        ExportArray::F32Borrowed2D(arr) => {
            let py_arr = PyArray2::from_array_bound(py, arr);
            dict.set_item("data", py_arr)?;
        }
        ExportArray::F32Owned1D(v) => {
            let py_arr = PyArray1::from_slice_bound(py, v);
            dict.set_item("data", py_arr)?;
        }
        ExportArray::F64Owned1D(v) => {
            let py_arr = PyArray1::from_slice_bound(py, v);
            dict.set_item("data", py_arr)?;
        }
        ExportArray::U32Owned1D(v) => {
            let py_arr = PyArray1::from_slice_bound(py, v);
            dict.set_item("data", py_arr)?;
        }
        ExportArray::U8Scalar(v) => {
            let py_arr = PyArray1::from_slice_bound(py, &[v.to_owned()]).get_item(0)?;
            dict.set_item("data", py_arr)?;
        }
    }

    // variable attrs
    let attrs_py = PyDict::new_bound(py);
    for (k, v) in &var.attrs {
        attrs_py.set_item(k, v.to_python(py)?)?;
    }
    dict.set_item("attrs", attrs_py)?;

    Ok(dict.into())
}

/// Helper: RFC3339 from seconds since epoch (UTC), safe unwrap for your data.
fn seconds_to_rfc3339(sec: f64) -> String {
    let ts = sec as i64;
    DateTime::<Utc>::from_timestamp(ts, 0).unwrap().to_rfc3339()
}

impl GPR {
    pub fn export_dataset(&self) -> Result<ExportDataset<'_>, String> {
        let width = self.width();
        let height = self.height();

        // ---------- Dimensions ----------
        let mut dims = BTreeMap::new();
        dims.insert("x".into(), width);
        dims.insert("y".into(), height);

        let mut has_topo = false;
        let mut topo_height = 0usize;
        if let Some(topo) = &self.topo_data {
            has_topo = true;
            topo_height = topo.shape()[0];
            dims.insert("y2".into(), topo_height);
        }

        // ---------- Global attributes ----------
        let mut attrs: BTreeMap<String, ExportAttr> = BTreeMap::new();

        // CF convention identifier
        attrs.insert("Conventions".into(), ExportAttr::String("CF-1.7".into()));

        // start/stop datetimes from track
        let start_dt = seconds_to_rfc3339(self.location.cor_points[0].time_seconds);
        let stop_dt = seconds_to_rfc3339(
            self.location.cor_points[self.location.cor_points.len() - 1].time_seconds,
        );

        attrs.insert("start_datetime".into(), ExportAttr::String(start_dt));
        attrs.insert("stop_datetime".into(), ExportAttr::String(stop_dt));
        attrs.insert(
            "processing_datetime".into(),
            ExportAttr::String(chrono::Local::now().to_rfc3339()),
        );
        attrs.insert(
            "program_version".into(),
            ExportAttr::String(format!(
                "{} version {} by {}",
                crate::PROGRAM_NAME,
                crate::PROGRAM_VERSION,
                crate::PROGRAM_AUTHORS
            )),
        );

        // user metadata (canonical JSON + flattened attributes)
        if !self.user_metadata.is_empty() {
            let canonical = user_metadata::canonical_json(&self.user_metadata)
                .unwrap_or_else(|_| "{}".to_string());

            attrs.insert(
                "ridal_user_metadata_json".into(),
                ExportAttr::String(canonical),
            );

            if let Ok(flat) = user_metadata::flatten_for_netcdf(&self.user_metadata) {
                for f in flat {
                    match f.value {
                        user_metadata::FlattenedMetadataValue::String(v) => {
                            attrs.insert(f.name, ExportAttr::String(v));
                        }
                        user_metadata::FlattenedMetadataValue::I64(v) => {
                            attrs.insert(f.name, ExportAttr::I64(v));
                        }
                        user_metadata::FlattenedMetadataValue::F64(v) => {
                            attrs.insert(f.name, ExportAttr::F64(v));
                        }
                        user_metadata::FlattenedMetadataValue::U8(v) => {
                            attrs.insert(f.name, ExportAttr::U8(v));
                        }
                    }
                }
            }
        }

        // antenna & acquisition metadata
        attrs.insert(
            "antenna".into(),
            ExportAttr::String(self.metadata.antenna.clone()),
        );
        attrs.insert(
            "antenna_separation".into(),
            ExportAttr::F32(self.metadata.antenna_separation),
        );
        attrs.insert(
            "antenna_separation_unit".into(),
            ExportAttr::String("m".into()),
        );
        attrs.insert(
            "frequency_steps".into(),
            ExportAttr::I64(self.metadata.frequency_steps as i64),
        );
        attrs.insert(
            "vertical_sampling_frequency".into(),
            ExportAttr::F32(self.metadata.frequency),
        );
        attrs.insert(
            "vertical_sampling_frequency_unit".into(),
            ExportAttr::String("MHz".into()),
        );
        if self.metadata.time_interval.is_finite() {
            attrs.insert(
                "time_interval".into(),
                ExportAttr::F32(self.metadata.time_interval),
            );
            attrs.insert("time_interval_unit".into(), ExportAttr::String("s".into()));
        }

        // processing log & steps
        attrs.insert(
            "processing_log".into(),
            ExportAttr::String(self.log.join("\n")),
        );
        attrs.insert(
            "processing_steps".into(),
            ExportAttr::Strings(self.steps.clone()),
        );

        // original filepaths:
        // first is the data_filepath, then any "Merged ..." from the log (as before)
        let mut filepaths = vec![self.metadata.data_filepath.to_string_lossy().to_string()];
        for log_str in &self.log {
            if let Some((_, filename)) = log_str.split_once("Merged ") {
                filepaths.push(filename.replace("\"", ""));
            }
        }
        attrs.insert("original_filepaths".into(), ExportAttr::Strings(filepaths));

        // medium velocity (+ unit)
        attrs.insert(
            "medium_velocity".into(),
            ExportAttr::F32(self.metadata.medium_velocity),
        );
        attrs.insert(
            "medium_velocity_unit".into(),
            ExportAttr::String("m / ns".into()),
        );

        // elevation correction
        let elev_corr = match &self.location.correction {
            LocationCorrection::None => "None".to_string(),
            LocationCorrection::Dem(fp) => format!(
                "DEM-corrected: {:?}",
                fp.as_path().file_name().unwrap().to_str().unwrap()
            ),
        };
        attrs.insert("elevation_correction".into(), ExportAttr::String(elev_corr));

        // CRS (kept as a global attr for now; CF grid_mapping deferred)
        attrs.insert("crs".into(), ExportAttr::String(self.location.crs.clone()));

        // total distance (+ unit)
        let distances = self.location.distances(); // f64
        if !distances.is_empty() {
            attrs.insert(
                "total_distance".into(),
                ExportAttr::F64(distances[distances.len() - 1]),
            );
            attrs.insert("total_distance_unit".into(), ExportAttr::String("m".into()));
        }

        // ---------- Coordinates ----------
        let mut coords: BTreeMap<String, ExportVariable<'_>> = BTreeMap::new();

        // x index
        coords.insert(
            "x".into(),
            ExportVariable {
                dims: vec!["x".into()],
                data: ExportArray::U32Owned1D((0u32..width as u32).collect()),
                attrs: [("long_name".into(), "trace index (zero-based)".into())]
                    .into_iter()
                    .collect(),
            },
        );

        // y index
        coords.insert(
            "y".into(),
            ExportVariable {
                dims: vec!["y".into()],
                data: ExportArray::U32Owned1D((0u32..height as u32).collect()),
                attrs: [("long_name".into(), "sample index (zero-based)".into())]
                    .into_iter()
                    .collect(),
            },
        );

        // distance (x)
        coords.insert(
            "distance".into(),
            ExportVariable {
                dims: vec!["x".into()],
                data: ExportArray::F64Owned1D(distances.to_vec()),
                attrs: [
                    ("units".into(), "m".into()),
                    ("long_name".into(), "distance along profile".into()),
                ]
                .into_iter()
                .collect(),
            },
        );

        // time (x) -- keep epoch-second values, but encode metadata in CF style
        let time_vals: Vec<f64> = self
            .location
            .cor_points
            .iter()
            .map(|p| p.time_seconds)
            .collect();
        coords.insert(
            "time".into(),
            ExportVariable {
                dims: vec!["x".into()],
                data: ExportArray::F64Owned1D(time_vals),
                attrs: [
                    (
                        "units".into(),
                        "seconds since 1970-01-01 00:00:00 UTC".into(),
                    ),
                    ("long_name".into(), "time".into()),
                    ("standard_name".into(), "time".into()),
                    ("axis".into(), "T".into()),
                    ("calendar".into(), "standard".into()),
                ]
                .into_iter()
                .collect(),
            },
        );

        // latitude / longitude (x) as auxiliary coordinates derived from native CRS

        // twtt (y) [ns]
        let twtt: Vec<f32> = {
            let step = self.vertical_resolution_ns();
            (0..height).map(|i| i as f32 * step).collect()
        };
        coords.insert(
            "twtt".into(),
            ExportVariable {
                dims: vec!["y".into()],
                data: ExportArray::F32Owned1D(twtt),
                attrs: [
                    ("units".into(), "ns".into()),
                    ("long_name".into(), "two-way travel time".into()),
                ]
                .into_iter()
                .collect(),
            },
        );

        // depth (y) [m]
        let depth_vals = self.depths().to_vec();
        coords.insert(
            "depth".into(),
            ExportVariable {
                dims: vec!["y".into()],
                data: ExportArray::F32Owned1D(depth_vals),
                attrs: [
                    ("units".into(), "m".into()),
                    (
                        "long_name".into(),
                        "depth assuming constant radar velocity".into(),
                    ),
                ]
                .into_iter()
                .collect(),
            },
        );

        // If topo_data present:
        // - y2 index
        // - topo_elevation(y2) [m] as linear ramp from max altitude down to min_alt - max_depth
        if has_topo {
            coords.insert(
                "y2".into(),
                ExportVariable {
                    dims: vec!["y2".into()],
                    data: ExportArray::U32Owned1D((0u32..topo_height as u32).collect()),
                    attrs: [(
                        "long_name".into(),
                        "topographically corrected sample index".into(),
                    )]
                    .into_iter()
                    .collect(),
                },
            );

            let (min_alt, max_alt) = self
                .location
                .cor_points
                .iter()
                .map(|p| p.altitude)
                .fold((f64::INFINITY, f64::NEG_INFINITY), |(mn, mx), v| {
                    (mn.min(v), mx.max(v))
                });

            let max_depth = self
                .depths()
                .iter()
                .cloned()
                .fold(f32::NEG_INFINITY, f32::max) as f64;

            let start = max_alt;
            let end = min_alt - max_depth;
            let topo_el: Vec<f64> = if topo_height == 1 {
                vec![start]
            } else {
                (0..topo_height)
                    .map(|i| {
                        let t = i as f64 / (topo_height - 1) as f64;
                        start + t * (end - start)
                    })
                    .collect()
            };

            coords.insert(
                "elevation_topocorr".into(),
                ExportVariable {
                    dims: vec!["y2".into()],
                    data: ExportArray::F64Owned1D(topo_el),
                    attrs: [
                        ("units".into(), "m".into()),
                        (
                            "long_name".into(),
                            "elevation axis for topographically corrected profile".into(),
                        ),
                    ]
                    .into_iter()
                    .collect(),
                },
            );
        }

        // ---------- Data variables ----------
        let mut data_vars: BTreeMap<String, ExportVariable<'_>> = BTreeMap::new();

        let grid_mapping =
            crate::coords::build_grid_mapping_from_crs(&self.location.crs)?.ok_or(format!(
                "CRS '{}' not supported. If it is a geographic CRS (e.g. WGS84 lat/lon), try a projected alternative.",
                self.location.crs
            ))?;
        // if grid_mapping.is_none() {
        //     eprintln!("Grid mapping construction failed. No grid_mapping variable exported");
        // };
        data_vars.insert(
            grid_mapping.variable_name.clone(),
            ExportVariable {
                dims: vec![],
                data: ExportArray::U8Scalar(0),
                attrs: grid_mapping.attrs.clone(),
            },
        );
        if let Some(crs_obj) = crate::coords::Crs::from_user_input(&self.location.crs).ok() {
            let native_coords: Vec<crate::coords::Coord> = self
                .location
                .cor_points
                .iter()
                .map(|p| crate::coords::Coord {
                    x: p.easting,
                    y: p.northing,
                })
                .collect();
            let wgs84_coords = crate::coords::to_wgs84(&native_coords, &crs_obj).ok();
            // This should never happen but I have this to be on the safe side.
            if wgs84_coords.is_none() {
                eprintln!("CRS conversion failed. Skipping longitude/latitude export");
            };
            if let Some(wgs84_coords) = wgs84_coords {
                let longitude_vals: Vec<f64> = wgs84_coords.iter().map(|p| p.x).collect();
                let latitude_vals: Vec<f64> = wgs84_coords.iter().map(|p| p.y).collect();

                coords.insert(
                    "longitude".into(),
                    ExportVariable {
                        dims: vec!["x".into()],
                        data: ExportArray::F64Owned1D(longitude_vals),
                        attrs: [
                            ("units".into(), "degrees_east".into()),
                            ("long_name".into(), "longitude".into()),
                            ("standard_name".into(), "longitude".into()),
                        ]
                        .into_iter()
                        .collect(),
                    },
                );

                coords.insert(
                    "latitude".into(),
                    ExportVariable {
                        dims: vec!["x".into()],
                        data: ExportArray::F64Owned1D(latitude_vals),
                        attrs: [
                            ("units".into(), "degrees_north".into()),
                            ("long_name".into(), "latitude".into()),
                            ("standard_name".into(), "latitude".into()),
                        ]
                        .into_iter()
                        .collect(),
                    },
                );
            }
            // easting, northing, elevation (x)
            let easting_vals: Vec<f64> =
                self.location.cor_points.iter().map(|p| p.easting).collect();
            let northing_vals: Vec<f64> = self
                .location
                .cor_points
                .iter()
                .map(|p| p.northing)
                .collect();
            let elevation_vals: Vec<f64> = self
                .location
                .cor_points
                .iter()
                .map(|p| p.altitude)
                .collect();

            let easting_attrs: BTreeMap<String, ExportAttr> = [
                ("units".into(), "m".into()),
                ("long_name".into(), "easting".into()),
                ("standard_name".into(), "projection_x_coordinate".into()),
            ]
            .into_iter()
            .collect();

            let northing_attrs: BTreeMap<String, ExportAttr> = [
                ("units".into(), "m".into()),
                ("long_name".into(), "northing".into()),
                ("standard_name".into(), "projection_y_coordinate".into()),
            ]
            .into_iter()
            .collect();

            coords.insert(
                "easting".into(),
                ExportVariable {
                    dims: vec!["x".into()],
                    data: ExportArray::F64Owned1D(easting_vals),
                    attrs: easting_attrs,
                },
            );

            coords.insert(
                "northing".into(),
                ExportVariable {
                    dims: vec!["x".into()],
                    data: ExportArray::F64Owned1D(northing_vals),
                    attrs: northing_attrs,
                },
            );

            coords.insert(
                "elevation".into(),
                ExportVariable {
                    dims: vec!["x".into()],
                    data: ExportArray::F64Owned1D(elevation_vals.clone()),
                    attrs: [
                        ("units".into(), "m".into()),
                        ("long_name".into(), "elevation".into()),
                    ]
                    .into_iter()
                    .collect(),
                },
            );
        } else {
            eprintln!("CRS conversion failed. Skipping longitude/latitude/grid_mapping export");
        }

        // main data (y, x)
        let mut dv_attrs = BTreeMap::new();
        dv_attrs.insert("units".into(), "mV".into());
        dv_attrs.insert("long_name".into(), "radar amplitude".into());
        dv_attrs.insert(
            "coordinates".into(),
            "distance time easting northing elevation longitude latitude elevation depth twtt"
                .into(),
        );

        dv_attrs.insert("grid_mapping".into(), "projected_crs".into());

        data_vars.insert(
            "data".into(),
            ExportVariable {
                dims: vec!["y".into(), "x".into()],
                data: ExportArray::F32Borrowed2D(&self.data),
                attrs: dv_attrs,
            },
        );

        // topo-corrected (y2, x)
        if let Some(topo) = &self.topo_data {
            let mut dv2_attrs = BTreeMap::new();
            dv2_attrs.insert("units".into(), "mV".into());
            dv2_attrs.insert(
                "long_name".into(),
                "topographically corrected radar amplitude".into(),
            );
            dv2_attrs.insert(
                "coordinates".into(),
                "distance time easting northing elevation longitude latitude elevation_topocorr"
                    .into(),
            );
            dv2_attrs.insert("grid_mapping".into(), "projected_crs".into());
            data_vars.insert(
                "data_topocorr".into(),
                ExportVariable {
                    dims: vec!["y2".into(), "x".into()],
                    data: ExportArray::F32Borrowed2D(topo),
                    attrs: dv2_attrs,
                },
            );
        }

        Ok(ExportDataset {
            dims,
            coords,
            data_vars,
            attrs,
        })
    }
}
