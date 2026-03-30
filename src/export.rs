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

/// Attribute values we want to support in the exported Dataset.
#[derive(Clone, Debug)]
pub enum ExportAttr {
    String(String),
    Strings(Vec<String>),
    F64(f64),
    F32(f32),
    I64(i64),
    U8(u8),
}

#[derive(Clone, Debug)]
pub enum ExportArray<'a> {
    F32Borrowed2D(&'a Array2<f32>),
    F32Owned1D(Vec<f32>),
    F64Owned1D(Vec<f64>),
    U32Owned1D(Vec<u32>),
}

#[derive(Clone, Debug)]
pub struct ExportVariable<'a> {
    pub dims: Vec<String>,
    pub data: ExportArray<'a>,
    /// Variable attributes (e.g., "unit", "coordinates")
    pub attrs: BTreeMap<String, String>,
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
            match v {
                ExportAttr::String(s) => attrs_py.set_item(k, s)?,
                ExportAttr::Strings(vs) => attrs_py.set_item(k, vs)?,

                ExportAttr::F64(x) => {
                    // This creates a numpy type and extracts the item
                    let f = PyArray1::from_slice_bound(py, &[*x]).get_item(0)?;
                    attrs_py.set_item(k, f)?
                }
                ExportAttr::F32(x) => {
                    let f = PyArray1::from_slice_bound(py, &[*x]).get_item(0)?;
                    attrs_py.set_item(k, f)?
                }
                ExportAttr::I64(x) => {
                    let f = PyArray1::from_slice_bound(py, &[*x]).get_item(0)?;
                    attrs_py.set_item(k, f)?
                }
                ExportAttr::U8(x) => {
                    let f = PyArray1::from_slice_bound(py, &[*x]).get_item(0)?;
                    attrs_py.set_item(k, f)?
                }
            }
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
    }

    // variable attrs
    let attrs_py = PyDict::new_bound(py);
    for (k, v) in &var.attrs {
        attrs_py.set_item(k, v)?;
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
    /// Build the full export dataset (ALL variables/coords/attrs except the
    /// two "Group 1" NetCDF-only attrs: processing-datetime and program-version).
    pub fn export_dataset(&self) -> ExportDataset<'_> {
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

        // start/stop datetimes from track
        let start_dt = seconds_to_rfc3339(self.location.cor_points[0].time_seconds);
        let stop_dt = seconds_to_rfc3339(
            self.location.cor_points[self.location.cor_points.len() - 1].time_seconds,
        );
        attrs.insert("start-datetime".into(), ExportAttr::String(start_dt));
        attrs.insert("stop-datetime".into(), ExportAttr::String(stop_dt));

        // user metadata (canonical JSON + flattened attributes)
        if !self.user_metadata.is_empty() {
            let canonical = user_metadata::canonical_json(&self.user_metadata)
                .unwrap_or_else(|_| "{}".to_string());
            attrs.insert(
                "ridal-user-metadata-json".into(),
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
            "antenna-separation".into(),
            ExportAttr::F32(self.metadata.antenna_separation),
        );
        attrs.insert(
            "frequency-steps".into(),
            ExportAttr::I64(self.metadata.frequency_steps as i64),
        );
        attrs.insert(
            "vertical-sampling-frequency".into(),
            ExportAttr::F32(self.metadata.frequency),
        );
        if self.metadata.time_interval.is_finite() {
            attrs.insert(
                "time-interval".into(),
                ExportAttr::F32(self.metadata.time_interval),
            );
        }

        // processing-log & processing-steps
        attrs.insert(
            "processing-log".into(),
            ExportAttr::String(self.log.join("\n")),
        );
        attrs.insert(
            "processing-steps".into(),
            ExportAttr::Strings(self.steps.clone()),
        );

        // original-filepaths:
        // first is the data_filepath, then any "Merged ..." from the log (as before)
        let mut filepaths = vec![self.metadata.data_filepath.to_string_lossy().to_string()];
        for log_str in &self.log {
            if let Some((_, filename)) = log_str.split_once("Merged ") {
                filepaths.push(filename.replace("\"", ""));
            }
        }
        attrs.insert("original-filepaths".into(), ExportAttr::Strings(filepaths));

        // medium-velocity (+ unit)
        attrs.insert(
            "medium-velocity".into(),
            ExportAttr::F32(self.metadata.medium_velocity),
        );
        attrs.insert(
            "medium-velocity-unit".into(),
            ExportAttr::String("m / ns".into()),
        );

        // elevation-correction
        let elev_corr = match &self.location.correction {
            LocationCorrection::None => "None".to_string(),
            LocationCorrection::Dem(fp) => format!(
                "DEM-corrected: {:?}",
                fp.as_path().file_name().unwrap().to_str().unwrap()
            ),
        };
        attrs.insert("elevation-correction".into(), ExportAttr::String(elev_corr));

        // crs
        attrs.insert("crs".into(), ExportAttr::String(self.location.crs.clone()));

        // total-distance (+ unit)
        let distances = self.location.distances(); // f64
        if !distances.is_empty() {
            attrs.insert(
                "total-distance".into(),
                ExportAttr::F64(distances[distances.len() - 1]),
            );
            attrs.insert("total-distance-unit".into(), ExportAttr::String("m".into()));
        }

        // NOTE: "program-version" and "processing-datetime" are NOT added here,
        // per your instruction to keep Group 1 exactly like before.
        // The NetCDF writer in io.rs will add them.

        // ---------- Coordinates ----------
        let mut coords: BTreeMap<String, ExportVariable<'_>> = BTreeMap::new();

        // x, y indices
        coords.insert(
            "x".into(),
            ExportVariable {
                dims: vec!["x".into()],
                data: ExportArray::U32Owned1D((0u32..width as u32).collect()),
                attrs: BTreeMap::new(),
            },
        );
        coords.insert(
            "y".into(),
            ExportVariable {
                dims: vec!["y".into()],
                data: ExportArray::U32Owned1D((0u32..height as u32).collect()),
                attrs: BTreeMap::new(),
            },
        );

        // distance (x)
        coords.insert(
            "distance".into(),
            ExportVariable {
                dims: vec!["x".into()],
                data: ExportArray::F64Owned1D(distances.to_vec()),
                attrs: [("unit".into(), "m".into())].into_iter().collect(),
            },
        );

        // time (x)
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
                attrs: [("unit".into(), "s".into())].into_iter().collect(),
            },
        );

        // easting, northing, elevation (x)
        let easting_vals: Vec<f64> = self.location.cor_points.iter().map(|p| p.easting).collect();
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
        coords.insert(
            "easting".into(),
            ExportVariable {
                dims: vec!["x".into()],
                data: ExportArray::F64Owned1D(easting_vals),
                attrs: [("unit".into(), "m".into())].into_iter().collect(),
            },
        );
        coords.insert(
            "northing".into(),
            ExportVariable {
                dims: vec!["x".into()],
                data: ExportArray::F64Owned1D(northing_vals),
                attrs: [("unit".into(), "m".into())].into_iter().collect(),
            },
        );
        coords.insert(
            "elevation".into(),
            ExportVariable {
                dims: vec!["x".into()],
                data: ExportArray::F64Owned1D(elevation_vals.clone()),
                attrs: [("unit".into(), "m a.s.l.".into())].into_iter().collect(),
            },
        );

        // return-time (y) [ns]
        let rt: Vec<f32> = {
            let step = self.vertical_resolution_ns();
            (0..height).map(|i| i as f32 * step).collect()
        };
        coords.insert(
            "return-time".into(),
            ExportVariable {
                dims: vec!["y".into()],
                data: ExportArray::F32Owned1D(rt),
                attrs: [("unit".into(), "ns".into())].into_iter().collect(),
            },
        );

        // depth (y) [m]
        let depth_vals = self.depths().to_vec();
        coords.insert(
            "depth".into(),
            ExportVariable {
                dims: vec!["y".into()],
                data: ExportArray::F32Owned1D(depth_vals),
                attrs: [("unit".into(), "m".into())].into_iter().collect(),
            },
        );

        // If topo_data present:
        //   - y2 index
        //   - topo_elevation(y2) [m] as linear ramp from max altitude down to min_alt - max_depth
        if has_topo {
            coords.insert(
                "y2".into(),
                ExportVariable {
                    dims: vec!["y2".into()],
                    data: ExportArray::U32Owned1D((0u32..topo_height as u32).collect()),
                    attrs: BTreeMap::new(),
                },
            );

            // topo_elevation
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
                "topo_elevation".into(),
                ExportVariable {
                    dims: vec!["y2".into()],
                    data: ExportArray::F64Owned1D(topo_el),
                    attrs: [("unit".into(), "m".into())].into_iter().collect(),
                },
            );
        }

        // ---------- Data variables ----------
        let mut data_vars: BTreeMap<String, ExportVariable<'_>> = BTreeMap::new();

        // main data (y, x)
        let mut dv_attrs = BTreeMap::new();
        dv_attrs.insert("unit".into(), "mV".into());
        dv_attrs.insert("coordinates".into(), "distance return-time".into());
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
            dv2_attrs.insert("unit".into(), "mV".into());
            dv2_attrs.insert("coordinates".into(), "distance topo_elevation".into());
            data_vars.insert(
                "data_topographically_corrected".into(),
                ExportVariable {
                    dims: vec!["y2".into(), "x".into()],
                    data: ExportArray::F32Borrowed2D(topo),
                    attrs: dv2_attrs,
                },
            );
        }

        ExportDataset {
            dims,
            coords,
            data_vars,
            attrs,
        }
    }
}
