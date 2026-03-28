//! # ridal --- Speeding up Ground Penetrating Radar (GPR) processing
//! A Ground Penetrating Radar (GPR) processing tool written in rust.

#[cfg(feature = "python")]
use pyo3::prelude::*;

mod cli;
mod coords;
mod dem;
mod filters;
mod formats;
mod gpr;
mod io;
mod metadata;
mod tools;

#[allow(dead_code)]
const PROGRAM_VERSION: &str = env!("CARGO_PKG_VERSION");
#[allow(dead_code)]
const PROGRAM_NAME: &str = env!("CARGO_PKG_NAME");
#[allow(dead_code)]
const PROGRAM_AUTHORS: &str = env!("CARGO_PKG_AUTHORS");

#[cfg(feature = "python")]
#[pymodule]
pub mod ridal {
    use crate::{formats, gpr};
    use pyo3::exceptions::{PyNotImplementedError, PyRuntimeError, PyValueError};
    use pyo3::prelude::*;
    use pyo3::types::{PyAny, PyDict, PyList, PyTuple};

    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn optional_metadata(
        py: Python<'_>,
        value: Option<PyObject>,
    ) -> PyResult<crate::metadata::UserMetadata> {
        match value {
            None => Ok(crate::metadata::UserMetadata::new()),
            Some(obj) => {
                let bound = obj.bind(py);
                let json = py.import_bound("json")?;
                let text: String = json.getattr("dumps")?.call1((bound,))?.extract()?;
                let value: serde_json::Value =
                    serde_json::from_str(&text).map_err(pyo3::exceptions::PyValueError::new_err)?;
                crate::metadata::value_to_metadata(value)
                    .map_err(pyo3::exceptions::PyValueError::new_err)
            }
        }
    }

    fn json_to_py(py: Python<'_>, text: &str) -> PyResult<PyObject> {
        let json = py.import_bound("json")?;
        Ok(json.getattr("loads")?.call1((text,))?.unbind())
    }

    fn fspath(py: Python<'_>, value: &Bound<'_, PyAny>) -> PyResult<PathBuf> {
        let os = py.import_bound("os")?;
        let path = os.getattr("fspath")?.call1((value,))?;
        let path_str: String = path.extract()?;

        let os_path = os.getattr("path")?;
        let expanded = os_path.getattr("expanduser")?.call1((path_str,))?;
        let expanded_str: String = expanded.extract()?;

        Ok(PathBuf::from(expanded_str))
    }

    fn inputs_to_paths(py: Python<'_>, value: &Bound<'_, PyAny>) -> PyResult<Vec<PathBuf>> {
        if let Ok(list) = value.downcast::<PyList>() {
            return list.iter().map(|item| fspath(py, &item)).collect();
        }
        if let Ok(tuple) = value.downcast::<PyTuple>() {
            return tuple.iter().map(|item| fspath(py, &item)).collect();
        }
        Ok(vec![fspath(py, value)?])
    }

    fn optional_path(py: Python<'_>, value: Option<PyObject>) -> PyResult<Option<PathBuf>> {
        match value {
            Some(obj) => {
                let bound = obj.bind(py);
                Ok(Some(fspath(py, &bound)?))
            }
            None => Ok(None),
        }
    }

    #[pymodule_init]
    fn init(m: &Bound<'_, PyModule>) -> PyResult<()> {
        let py = m.py();
        m.add("version", crate::PROGRAM_VERSION)?;
        m.add("__version__", crate::PROGRAM_VERSION)?;

        let all_steps = gpr::all_available_steps();
        let step_names = all_steps
            .iter()
            .map(|(name, _)| name.clone())
            .collect::<Vec<String>>();
        let step_descriptions = all_steps
            .iter()
            .map(|(name, description)| (name.clone(), description.clone()))
            .collect::<BTreeMap<String, String>>();

        m.add("all_steps", step_names)?;
        m.add(
            "all_step_descriptions",
            json_to_py(py, &serde_json::to_string(&step_descriptions).unwrap())?,
        )?;

        let all_formats = formats::all_formats();
        let format_names = all_formats
            .iter()
            .map(|fmt| fmt.name.to_string())
            .collect::<Vec<String>>();

        let format_descriptions = all_formats
            .iter()
            .map(|fmt| {
                (
                    fmt.name.to_string(),
                    serde_json::json!({
                        "description": fmt.description,
                        "capabilities": {
                            "read": fmt.capabilities.read,
                            "write": fmt.capabilities.write,
                        },
                        "files": {
                            "header": fmt.files.header,
                            "data": fmt.files.data,
                            "coordinates": fmt.files.coordinates,
                        }
                    }),
                )
            })
            .collect::<BTreeMap<String, serde_json::Value>>();

        m.add("all_formats", format_names)?;
        m.add(
            "all_format_descriptions",
            json_to_py(py, &serde_json::to_string(&format_descriptions).unwrap())?,
        )?;
        Ok(())
    }

    /// Process one or more GPR files.
    ///
    /// Parameters
    /// ----------
    /// inputs
    ///     A path-like object or a list/tuple of path-like objects.
    /// output
    ///     Output filename or directory. If None, a default .nc path is derived from the first input.
    /// steps
    ///     Processing steps to run, either as a comma-separated string or a list of strings.
    /// default
    ///     Use the default processing profile.
    /// default_with_topo
    ///     Use the default processing profile plus topographic correction.
    #[pyfunction]
    #[pyo3(signature = (
        inputs,
        output=None,
        steps=None,
        default=false,
        default_with_topo=false,
        velocity=0.168,
        cor=None,
        dem=None,
        crs=None,
        track=None,
        quiet=false,
        render=None,
        no_export=false,
        override_antenna_mhz=None,
        metadata=None,
    ))]
    fn process(
        py: Python<'_>,
        inputs: PyObject,
        output: Option<PyObject>,
        steps: Option<PyObject>,
        default: bool,
        default_with_topo: bool,
        velocity: f32,
        cor: Option<PyObject>,
        dem: Option<PyObject>,
        crs: Option<String>,
        track: Option<PyObject>,
        quiet: bool,
        render: Option<PyObject>,
        no_export: bool,
        override_antenna_mhz: Option<f32>,
        metadata: Option<PyObject>,
    ) -> PyResult<String> {
        let input_paths = inputs_to_paths(py, &inputs.bind(py))?;
        let output_path = optional_path(py, output)?;
        let cor_path = optional_path(py, cor)?;
        let dem_path = optional_path(py, dem)?;
        let track_path = match track {
            Some(obj) => Some(Some(fspath(py, &obj.bind(py))?)),
            None => None,
        };
        let render_path = match render {
            Some(obj) => Some(Some(fspath(py, &obj.bind(py))?)),
            None => None,
        };

        let steps_text = match steps {
            Some(step_obj) => {
                let bound = step_obj.bind(py);
                if let Ok(step_text) = bound.extract::<String>() {
                    Some(step_text)
                } else if let Ok(step_list) = bound.downcast::<PyList>() {
                    let parts = step_list
                        .iter()
                        .map(|item| item.extract::<String>())
                        .collect::<PyResult<Vec<String>>>()?;
                    Some(parts.join(","))
                } else if let Ok(step_tuple) = bound.downcast::<PyTuple>() {
                    let parts = step_tuple
                        .iter()
                        .map(|item| item.extract::<String>())
                        .collect::<PyResult<Vec<String>>>()?;
                    Some(parts.join(","))
                } else {
                    return Err(PyValueError::new_err(
                        "steps must be a string or a list/tuple of strings",
                    ));
                }
            }
            None => None,
        };

        let resolved_steps = if default_with_topo {
            let mut profile = gpr::default_processing_profile();
            profile.push("correct_topography".to_string());
            profile
        } else if default {
            gpr::default_processing_profile()
        } else if let Some(step_text) = steps_text.as_deref() {
            crate::tools::parse_step_list(step_text).map_err(PyValueError::new_err)?
        } else {
            vec![]
        };

        gpr::validate_steps(&resolved_steps).map_err(PyValueError::new_err)?;

        let user_metadata = optional_metadata(py, metadata)?;

        let params = gpr::RunParams {
            filepaths: input_paths,
            output_path,
            dem_path,
            cor_path,
            medium_velocity: velocity,
            crs,
            quiet,
            track_path,
            steps: resolved_steps,
            no_export,
            render_path,
            override_antenna_mhz,
            user_metadata,
        };

        let result = gpr::run(params).map_err(|e| PyRuntimeError::new_err(format!("{e:?}")))?;
        Ok(result.output_path.to_string_lossy().to_string())
    }
    /// Batch-process one or more GPR files into many outputs.
    ///
    /// Parameters
    /// ----------
    /// inputs
    /// A path-like object or a list/tuple of path-like objects.
    /// output
    /// Output directory (must already exist).
    /// steps
    /// Processing steps to run, either as a comma-separated string or a list of strings.
    /// default
    /// Use the default processing profile.
    /// default_with_topo
    /// Use the default processing profile plus topographic correction.
    /// merge
    /// Optional merge threshold (e.g. "10 min"). Neighboring chronological profiles
    /// closer than this threshold will be merged if compatible.
    /// metadata
    /// Optional user metadata mapping attached independently to every produced output.
    #[pyfunction]
    #[pyo3(signature = (
    inputs,
    output,
    steps=None,
    default=false,
    default_with_topo=false,
    velocity=0.168,
    cor=None,
    dem=None,
    crs=None,
    track=None,
    quiet=false,
    render=None,
    no_export=false,
    merge=None,
    override_antenna_mhz=None,
    metadata=None,
))]
    fn batch_process(
        py: Python<'_>,
        inputs: PyObject,
        output: PyObject,
        steps: Option<PyObject>,
        default: bool,
        default_with_topo: bool,
        velocity: f32,
        cor: Option<PyObject>,
        dem: Option<PyObject>,
        crs: Option<String>,
        track: Option<PyObject>,
        quiet: bool,
        render: Option<PyObject>,
        no_export: bool,
        merge: Option<String>,
        override_antenna_mhz: Option<f32>,
        metadata: Option<PyObject>,
    ) -> PyResult<Vec<String>> {
        use pyo3::exceptions::{PyRuntimeError, PyValueError};

        let input_paths = inputs_to_paths(py, &inputs.bind(py))?;
        let output_dir = fspath(py, &output.bind(py))?;
        if !output_dir.is_dir() {
            return Err(PyValueError::new_err(format!(
                "output must be an existing directory in batch_process(): {}",
                output_dir.display()
            )));
        }

        let cor_path = optional_path(py, cor)?;
        let dem_path = optional_path(py, dem)?;
        let track_dir = match track {
            Some(obj) => {
                let p = fspath(py, &obj.bind(py))?;
                if !p.is_dir() {
                    return Err(PyValueError::new_err(format!(
                        "track must be an existing directory in batch_process(): {}",
                        p.display()
                    )));
                }
                Some(p)
            }
            None => None,
        };
        let render_dir = match render {
            Some(obj) => {
                let p = fspath(py, &obj.bind(py))?;
                if !p.is_dir() {
                    return Err(PyValueError::new_err(format!(
                        "render must be an existing directory in batch_process(): {}",
                        p.display()
                    )));
                }
                Some(p)
            }
            None => None,
        };

        let steps_text = match steps {
            Some(step_obj) => {
                let bound = step_obj.bind(py);
                if let Ok(step_text) = bound.extract::<String>() {
                    Some(step_text)
                } else if let Ok(step_list) = bound.downcast::<PyList>() {
                    let parts = step_list
                        .iter()
                        .map(|item| item.extract::<String>())
                        .collect::<PyResult<Vec<String>>>()?;
                    Some(parts.join(","))
                } else if let Ok(step_tuple) = bound.downcast::<PyTuple>() {
                    let parts = step_tuple
                        .iter()
                        .map(|item| item.extract::<String>())
                        .collect::<PyResult<Vec<String>>>()?;
                    Some(parts.join(","))
                } else {
                    return Err(PyValueError::new_err(
                        "steps must be a string or a list/tuple of strings",
                    ));
                }
            }
            None => None,
        };

        let resolved_steps = if default_with_topo {
            let mut profile = gpr::default_processing_profile();
            profile.push("correct_topography".to_string());
            profile
        } else if default {
            gpr::default_processing_profile()
        } else if let Some(step_text) = steps_text.as_deref() {
            crate::tools::parse_step_list(step_text).map_err(PyValueError::new_err)?
        } else {
            vec![]
        };

        gpr::validate_steps(&resolved_steps).map_err(PyValueError::new_err)?;
        let user_metadata = optional_metadata(py, metadata)?;

        let params = gpr::BatchRunParams {
            filepaths: input_paths,
            output_dir,
            dem_path,
            cor_path,
            medium_velocity: velocity,
            crs,
            quiet,
            track_dir,
            steps: resolved_steps,
            no_export,
            render_dir,
            merge,
            override_antenna_mhz,
            user_metadata,
        };

        let result =
            gpr::run_batch(params).map_err(|e| PyRuntimeError::new_err(format!("{e:?}")))?;

        Ok(result
            .output_paths
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect())
    }

    /// Inspect one or more GPR files.
    ///
    /// Returns a dictionary for one input and a list of dictionaries for many inputs.
    #[pyfunction]
    #[pyo3(signature = (
        inputs,
        velocity=0.168,
        cor=None,
        dem=None,
        crs=None,
        override_antenna_mhz=None,
    ))]
    fn info(
        py: Python<'_>,
        inputs: PyObject,
        velocity: f32,
        cor: Option<PyObject>,
        dem: Option<PyObject>,
        crs: Option<String>,
        override_antenna_mhz: Option<f32>,
    ) -> PyResult<PyObject> {
        let input_paths = inputs_to_paths(py, &inputs.bind(py))?;
        let cor_path = optional_path(py, cor)?;
        let dem_path = optional_path(py, dem)?;

        let params = gpr::InfoParams {
            filepaths: input_paths,
            dem_path,
            cor_path,
            medium_velocity: velocity,
            crs,
            override_antenna_mhz,
        };
        let records =
            gpr::inspect(params).map_err(|e| PyRuntimeError::new_err(format!("{e:?}")))?;

        if records.len() == 1 {
            json_to_py(py, &serde_json::to_string(&records[0]).unwrap())
        } else {
            json_to_py(py, &serde_json::to_string(&records).unwrap())
        }
    }

    /// Legacy compatibility shim for the removed CLI-in-Python interface.
    #[pyfunction(signature = (*_args, **_kwargs))]
    fn run_cli(_args: &Bound<'_, PyTuple>, _kwargs: Option<&Bound<'_, PyDict>>) -> PyResult<()> {
        Err(PyNotImplementedError::new_err(
            "ridal.run_cli() has been removed. Use ridal.process(...) for processing and ridal.info(...) for metadata inspection.",
        ))
    }
}
