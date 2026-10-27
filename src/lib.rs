//! # ridal --- Speeding up Ground Penetrating Radar (GPR) processing
//! A Ground Penetrating Radar (GPR) processing tool written in rust.

mod cli;
mod coords;
mod dem;
mod export;
/// A radargram built from arrays: `ridal.write_netcdf` in Python, its only
/// caller outside tests.
#[cfg_attr(not(feature = "python"), allow(dead_code))]
mod external;
mod formats;
mod gpr;
/// Radargram id and display name patterns for batches (#368).
mod id_pattern;
mod identity;
mod inspect;
mod interp;
mod io;
/// One processing job, run in a child process for the server (#382).
mod job;
mod positions;
mod processing;
mod project;
/// What a processed radargram was made from (#382): raw file checksums,
/// settings and versioned steps, hashed into its revision id.
mod recipe;
/// Radargram rendering: source window -> view -> resample -> normalize ->
/// colormap -> encode. Outside the `server` feature because nothing in it
/// is about HTTP -- the CLI's `ridal render` and the web GUI are two
/// callers of one pipeline, and `image` was already an unconditional
/// dependency, so this costs a CLI-only build nothing.
mod render;
#[cfg(feature = "server")]
mod server;
/// A site: server-wide identity and hosting for one or more projects (#214).
/// Outside the `server` feature because the CLI creates and manages sites.
mod site;
/// Windowed NetCDF reads for the renderer above. Moved out of `server`
/// with it: a chunk of a radargram is not a server concept.
mod source;
/// Typical trace spacing and survey speed, for the viewer's metadata
/// panel (#412); only the server shows them.
#[cfg(feature = "server")]
mod survey;
mod tools;
mod user_metadata;

#[allow(dead_code)]
const PROGRAM_VERSION: &str = env!("CARGO_PKG_VERSION");
#[allow(dead_code)]
const PROGRAM_NAME: &str = env!("CARGO_PKG_NAME");
#[allow(dead_code)]
const PROGRAM_AUTHORS: &str = env!("CARGO_PKG_AUTHORS");

/// Python interface for ridal.
///
/// ridal provides fast, Rust-backed tools for reading, inspecting, and
/// processing ground-penetrating radar (GPR) data from Python.
///
/// Main entry points
/// -----------------
/// read(...)
///     Read one or more GPR files into memory without applying a processing
///     workflow.
/// info(...)
///     Inspect one or more GPR files and return metadata summaries.
/// coords(...)
///     Write a recording's positions as a coordinate CSV, from partial
///     positions, a GNSS track, a DEM and a start time.
/// process(...)
///     Process one or more GPR files into a single output or an in-memory
///     dataset.
/// batch_process(...)
///     Batch-process one or more GPR files into multiple outputs.
/// render(...)
///     Render a processed radargram to an image file.
///
/// Discovery helpers
/// -----------------
/// all_steps, all_step_descriptions
///     Available processing steps and their descriptions.
/// all_formats, all_format_descriptions
///     Supported file formats and their capabilities.
/// version, __version__
///     Installed ridal version.
///
/// Notes
/// -----
/// `xarray` is an optional dependency. If it is installed, some functions can
/// return `xarray.Dataset` objects. Otherwise, use the plain Python dataset
/// representations such as `"xarray_dict"`.
#[cfg(feature = "python")]
#[pyo3::pymodule(name = "_ridal")]
pub mod ridal {
    use crate::{formats, gpr};
    use numpy::PyArrayMethods;
    use pyo3::exceptions::{PyNotImplementedError, PyRuntimeError};
    use pyo3::ffi::c_str;
    use pyo3::prelude::*;
    use pyo3::types::{PyAny, PyBool, PyDict, PyList, PyTuple};

    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn optional_metadata(
        py: Python<'_>,
        value: Option<Py<PyAny>>,
    ) -> PyResult<crate::user_metadata::UserMetadata> {
        match value {
            None => Ok(crate::user_metadata::UserMetadata::new()),
            Some(obj) => {
                let bound = obj.bind(py);
                let json = py.import("json")?;
                let text: String = json.getattr("dumps")?.call1((bound,))?.extract()?;
                let value: serde_json::Value = serde_json::from_str(&text)
                    .map_err(|e| pyo3::exceptions::PyValueError::new_err(format!("{e:?}")))?;
                crate::user_metadata::value_to_metadata(value)
                    .map_err(pyo3::exceptions::PyValueError::new_err)
            }
        }
    }

    fn json_to_py(py: Python<'_>, text: &str) -> PyResult<Py<PyAny>> {
        let json = py.import("json")?;
        Ok(json.getattr("loads")?.call1((text,))?.unbind())
    }

    fn xarray_dict_to_ds(py: Python<'_>, dict: Py<PyAny>) -> PyResult<Py<PyAny>> {
        let xarray = py.import("xarray")?;

        Ok(xarray
            .getattr("Dataset")?
            .getattr("from_dict")?
            .call1((&dict,))?
            .unbind())
    }

    fn fspath(py: Python<'_>, value: &Bound<'_, PyAny>) -> PyResult<PathBuf> {
        let os = py.import("os")?;
        let path = os.getattr("fspath")?.call1((value,))?;
        let path_str: String = path.extract()?;

        let os_path = os.getattr("path")?;
        let expanded = os_path.getattr("expanduser")?.call1((path_str,))?;
        let expanded_str: String = expanded.extract()?;

        Ok(PathBuf::from(expanded_str))
    }

    fn inputs_to_paths(py: Python<'_>, value: &Bound<'_, PyAny>) -> PyResult<Vec<PathBuf>> {
        if let Ok(list) = value.cast::<PyList>() {
            return list.iter().map(|item| fspath(py, &item)).collect();
        }
        if let Ok(tuple) = value.cast::<PyTuple>() {
            return tuple.iter().map(|item| fspath(py, &item)).collect();
        }
        Ok(vec![fspath(py, value)?])
    }

    fn optional_path(py: Python<'_>, value: Option<Py<PyAny>>) -> PyResult<Option<PathBuf>> {
        match value {
            Some(obj) => {
                let bound = obj.bind(py);
                Ok(Some(fspath(py, bound)?))
            }
            None => Ok(None),
        }
    }

    /// A side output's `path-like or bool` argument, as the CLI's optional
    /// value: `True` is `Some(None)`, a path beside the output derived later,
    /// and `False` is the same as leaving it out (#93).
    fn path_or_flag(py: Python<'_>, value: Option<Py<PyAny>>) -> PyResult<Option<Option<PathBuf>>> {
        let Some(obj) = value else {
            return Ok(None);
        };
        let bound = obj.bind(py);
        if let Ok(flag) = bound.cast::<PyBool>() {
            return Ok(flag.is_true().then_some(None));
        }
        Ok(Some(Some(fspath(py, bound)?)))
    }

    /// The `steps=` argument as the CLI's `--steps` text: a string as it is,
    /// or a list or tuple of steps joined with commas.
    fn steps_text(py: Python<'_>, steps: Option<Py<PyAny>>) -> PyResult<Option<String>> {
        let Some(steps) = steps else {
            return Ok(None);
        };
        let bound = steps.bind(py);
        if let Ok(text) = bound.extract::<String>() {
            return Ok(Some(text));
        }
        let parts: PyResult<Vec<String>> = if let Ok(list) = bound.cast::<PyList>() {
            list.iter().map(|item| item.extract::<String>()).collect()
        } else if let Ok(tuple) = bound.cast::<PyTuple>() {
            tuple.iter().map(|item| item.extract::<String>()).collect()
        } else {
            return Err(pyo3::exceptions::PyValueError::new_err(
                "steps must be a string or a list/tuple of strings",
            ));
        };
        Ok(Some(parts?.join(",")))
    }

    /// `override_date=`: a `datetime.date` or a `"YYYY-MM-DD"` string. A
    /// `datetime.datetime` is refused rather than cut to its date, since its
    /// time zone could make that the wrong day.
    fn date_arg(py: Python<'_>, value: Option<Py<PyAny>>) -> PyResult<Option<chrono::NaiveDate>> {
        use pyo3::exceptions::{PyTypeError, PyValueError};
        let Some(value) = value else {
            return Ok(None);
        };
        let value = value.bind(py);
        let datetime = py.import("datetime")?;
        let text = if value.is_instance(&datetime.getattr("datetime")?)? {
            return Err(PyTypeError::new_err(
                "override_date must be a date, not a datetime: give the UTC date, \
                 for example dt.date() after converting it to UTC",
            ));
        } else if value.is_instance(&datetime.getattr("date")?)? {
            value.call_method0("isoformat")?.extract::<String>()?
        } else if let Ok(text) = value.extract::<String>() {
            text
        } else {
            return Err(PyTypeError::new_err(
                "override_date must be a datetime.date or a \"YYYY-MM-DD\" string",
            ));
        };
        chrono::NaiveDate::parse_from_str(text.trim(), "%Y-%m-%d")
            .map(Some)
            .map_err(|e| {
                PyValueError::new_err(format!(
                    "override_date {text:?} is not a YYYY-MM-DD date: {e}"
                ))
            })
    }

    /// `lines=`: an int, a sequence of ints, or a string as `--lines` takes
    /// it (`"all"`, `"0,2-4"`).
    fn lines_arg(
        py: Python<'_>,
        value: Option<Py<PyAny>>,
    ) -> PyResult<Option<formats::LineSelection>> {
        use pyo3::exceptions::{PyTypeError, PyValueError};
        let Some(value) = value else {
            return Ok(None);
        };
        let value = value.bind(py);
        if let Ok(text) = value.extract::<String>() {
            return text
                .parse::<formats::LineSelection>()
                .map(Some)
                .map_err(PyValueError::new_err);
        }
        let lines = if let Ok(line) = value.extract::<u32>() {
            vec![line]
        } else if let Ok(lines) = value.extract::<Vec<u32>>() {
            lines
        } else {
            return Err(PyTypeError::new_err(
                "lines must be an int, a sequence of ints, or a string such as \"all\" or \"0,2-4\"",
            ));
        };
        // Through the parser, so a repeated or empty selection is refused
        // here as it is on the command line.
        let text = lines
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(",");
        text.parse::<formats::LineSelection>()
            .map(Some)
            .map_err(PyValueError::new_err)
    }

    /// The processing options `process()` and `batch_process()` share.
    #[allow(clippy::too_many_arguments)]
    fn processing_options(
        py: Python<'_>,
        steps: Option<Py<PyAny>>,
        default: bool,
        default_with_topo: bool,
        velocity: f32,
        cor: Option<Py<PyAny>>,
        dem: Option<Py<PyAny>>,
        crs: Option<String>,
        quiet: bool,
        render_profile: Option<String>,
        render_width: Option<usize>,
        render_topo: bool,
        no_export: bool,
        override_antenna_mhz: Option<f32>,
        override_antenna_separation: Option<f32>,
        override_date: Option<Py<PyAny>>,
        lines: Option<Py<PyAny>>,
        metadata: Option<Py<PyAny>>,
        group_name: Option<String>,
        group_id: Option<String>,
    ) -> PyResult<crate::processing::ProcessingOptions> {
        let steps = crate::processing::resolve_steps(
            default,
            default_with_topo,
            steps_text(py, steps)?.as_deref(),
        )
        .map_err(pyo3::exceptions::PyValueError::new_err)?;
        Ok(crate::processing::ProcessingOptions {
            dem_path: optional_path(py, dem)?,
            cor_path: optional_path(py, cor)?,
            medium_velocity: velocity,
            crs,
            quiet,
            steps,
            no_export,
            render_profile,
            render_width,
            render_topo,
            override_antenna_mhz,
            override_antenna_separation,
            override_date: date_arg(py, override_date)?,
            lines: lines_arg(py, lines)?,
            user_metadata: optional_metadata(py, metadata)?,
            group: group_name,
            group_id,
        })
    }

    /// A `batch_process()` side output's `path-like` argument: an existing
    /// directory.
    fn existing_dir(
        py: Python<'_>,
        value: Option<Py<PyAny>>,
        what: &str,
    ) -> PyResult<Option<PathBuf>> {
        let Some(path) = optional_path(py, value)? else {
            return Ok(None);
        };
        if !path.is_dir() {
            return Err(pyo3::exceptions::PyValueError::new_err(format!(
                "{what} must be an existing directory in batch_process(): {}",
                path.display()
            )));
        }
        Ok(Some(path))
    }

    #[pymodule_init]
    fn init(m: &Bound<'_, PyModule>) -> PyResult<()> {
        let py = m.py();
        m.add("version", crate::PROGRAM_VERSION)?;
        m.add("__version__", crate::PROGRAM_VERSION)?;

        let all_steps = crate::processing::steps::all_available_steps();
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

    /// Process one or more GPR files into a single output or an in-memory dataset.
    ///
    /// Use this function when you want to modify or process radar data. One or more
    /// input files are read and optionally corrected or transformed. By default,
    /// the result is written to a single output dataset and the output path is
    /// returned. If `return_dataset=True`, the processed result is returned in
    /// memory instead of being written to disk.
    ///
    /// Parameters
    /// ----------
    /// inputs : path-like or sequence of path-like
    ///     One or more input files to read. A single path, list, or tuple of
    ///     path-like objects is accepted.
    /// output : path-like, optional
    ///     Output file path or output directory. If omitted, a default output path
    ///     is derived from the first input. Ignored when `return_dataset=True`.
    /// steps : str or sequence of str, optional
    ///     Processing steps to apply. This may be given either as a comma-separated
    ///     string or as a sequence of step names.
    ///
    ///     Available steps are exposed as `ridal.all_steps`, and descriptions are
    ///     available in `ridal.all_step_descriptions`.
    ///
    ///     Exactly one of `steps`, `default`, and `default_with_topo` may be given.
    /// return_dataset : bool, default False
    ///     If True, return the processed data as an in-memory dataset object instead
    ///     of writing the dataset to disk. In this mode, `output`, `render`, and
    ///     `track` must not be provided.
    /// default : bool, default False
    ///     Use the default processing profile.
    ///
    ///     Exactly one of `steps`, `default`, and `default_with_topo` may be given.
    /// default_with_topo : bool, default False
    ///     Use the default processing profile followed by `correct_topography`,
    ///     which stores a corrected copy of the data and more than doubles the
    ///     size of the file. A corrected image needs only `render_topo=True`.
    ///
    ///     Exactly one of `steps`, `default`, and `default_with_topo` may be given.
    /// velocity : float, default 0.168
    ///     Propagation velocity in meters per nanosecond.
    /// cor : path-like, optional
    ///     Positions to use instead of the recording's own. A file (the
    ///     format's own positions file, such as a ``.cor``, or a Ridal
    ///     coordinate CSV of ``trace,time,lon,lat[,alt]``) for one recording
    ///     or the lines of one IceRadar file, or a directory holding ``<stem>.coords.csv`` or the format's own
    ///     positions file for each. Without it, a ``<stem>.coords.csv``
    ///     beside a recording is used before its own positions file.
    /// dem : path-like, optional
    ///     Digital elevation model to sample for topographic information.
    /// crs : str, optional
    ///     Coordinate reference system for interpreting or transforming coordinates.
    ///     If omitted, the most appropriate WGS84 UTM zone is used.
    /// track : path-like or bool, optional
    ///     Output path for exported track data, or `True` for
    ///     `<output stem>_track.csv` beside the output, as `ridal process
    ///     --track` without a value does. A directory gets that file name. Not
    ///     allowed when `return_dataset=True`.
    /// quiet : bool, default False
    ///     Reduce logging and progress output.
    /// render_profile : str, optional
    ///     Render profile for `render`: a built-in name, or a path to a TOML
    ///     file describing one. Defaults to the built-in `"default"`. Distinct
    ///     from the *processing* profile selected by `default=` and `steps=`.
    /// render_width : int, optional
    ///     Output width in pixels for `render`. Defaults to one pixel per
    ///     trace; a width beyond the trace count is clamped, not upsampled to.
    /// render_topo : bool, default False
    ///     Render the topographically corrected view for `render`, as
    ///     `ridal.render(..., topo=True)` does. Processing fails with a
    ///     `RuntimeError` when the radargram has no usable elevation and depth
    ///     axes.
    /// render : path-like or bool, optional
    ///     Output path for a rendered figure, or `True` for the output path
    ///     with a `.jpg` extension, as `ridal process -r` without a value does.
    ///     A directory gets that file name. Not allowed when
    ///     `return_dataset=True`.
    /// no_export : bool, default False
    ///     Run processing without writing the main dataset output. Side outputs such
    ///     as rendered figures or exported tracks may still be produced.
    /// override_antenna_mhz : float, optional
    ///     Override the antenna center frequency inferred from the input data.
    /// override_antenna_separation : float, optional
    ///     Override the antenna separation inferred from the input data.
    /// override_date : datetime.date or str, optional
    ///     The UTC date of recordings whose files record only a time of day,
    ///     or a date that may be wrong: IceRadar, whose date is the
    ///     acquisition PC's clock, and pulseEKKO. A ``datetime.date`` or a
    ///     ``"YYYY-MM-DD"`` string; a ``datetime.datetime`` is refused.
    /// lines : int, sequence of int or str, optional
    ///     Which lines of a file holding several (IceRadar) to read: one
    ///     (``3``), several merged in the order given (``[0, 2, 3]`` or
    ///     ``"0,2-3"``), or ``"all"``. Lines recorded with different settings
    ///     cannot be merged. Required for a file of more than one line, and
    ///     refused for formats without lines.
    /// metadata : mapping, optional
    ///     Additional user metadata to attach to the result. This should be a
    ///     JSON-serializable mapping. Root keys are interpreted as strings.
    /// radargram_id : str, optional
    ///     Stable, unique identifier for the output radargram (lowercase ASCII,
    ///     digits, ``-``, ``_``). Defaults to the output file stem when omitted.
    /// radargram_name : str, optional
    ///     Human-readable display label for the output radargram. Purely
    ///     cosmetic: it carries no identity semantics. Non-ASCII is accepted
    ///     and written to the NetCDF unchanged; see `group_name` for the
    ///     reader caveat.
    /// group_id : str, optional
    ///     Explicit identifier for the group the output belongs to, overriding
    ///     the id derived from `group_name`.
    /// group_name : str, optional
    ///     Human-readable name of the group the output belongs to (survey,
    ///     campaign, location), used for catalog grouping. Non-ASCII is
    ///     accepted and written unchanged. Ridal writes every attribute it
    ///     generates itself as ASCII; a value from here is the only thing that
    ///     can make the file non-ASCII, and some readers (for example xarray
    ///     with the h5netcdf engine) will garble it, because the attribute is
    ///     stored as ASCII-tagged bytes. An id is derived from it
    ///     automatically unless `group_id` is given.
    /// return_dataset_format : str, default "xarray_dict"
    ///     Format used when `return_dataset=True`.
    ///
    ///     Supported values currently include:
    ///
    ///     - ``"xarray_dict"`` for a plain Python representation that does not
    ///       require importing `xarray`.
    ///     - ``"xarray"`` for an `xarray.Dataset`, which requires `xarray` to be
    ///       installed.
    ///
    ///     More return formats may be added in the future.
    ///
    /// Returns
    /// -------
    /// str or object
    ///     The output dataset path as a string in normal export mode, or an
    ///     in-memory dataset object when `return_dataset=True`.
    ///
    /// Raises
    /// ------
    /// ValueError
    ///     If incompatible arguments are provided, including:
    ///
    ///     - more than one of `steps`, `default`, and `default_with_topo`
    ///     - `return_dataset=True` together with `output`, `render`, or `track`
    ///     - an invalid `steps` value
    /// RuntimeError
    ///     If processing fails.
    /// NotImplementedError
    ///     If `return_dataset_format` is not supported.
    ///
    /// Notes
    /// -----
    /// `process()` is the main processing entry point and is intended for workflows
    /// that modify the data. For lightweight loading of raw data without heavy
    /// processing, use `read()`.
    #[pyfunction]
    #[allow(clippy::too_many_arguments)]
    #[pyo3(signature = (
        inputs,
        output=None,
        *,
        steps=None,
        return_dataset=false,
        default=false,
        default_with_topo=false,
        velocity=0.168,
        cor=None,
        dem=None,
        crs=None,
        track=None,
        quiet=false,
        render=None,
        render_profile=None,
        render_width=None,
        render_topo=false,
        no_export=false,
        override_antenna_mhz=None,
        override_antenna_separation=None,
        override_date=None,
        lines=None,
        metadata=None,
        radargram_id=None,
        radargram_name=None,
        group_id=None,
        group_name=None,
        return_dataset_format="xarray_dict"
    ))]
    fn process(
        py: Python<'_>,
        inputs: Py<PyAny>,
        output: Option<Py<PyAny>>,
        steps: Option<Py<PyAny>>,
        return_dataset: bool,
        default: bool,
        default_with_topo: bool,
        velocity: f32,
        cor: Option<Py<PyAny>>,
        dem: Option<Py<PyAny>>,
        crs: Option<String>,
        track: Option<Py<PyAny>>,
        quiet: bool,
        render: Option<Py<PyAny>>,
        render_profile: Option<String>,
        render_width: Option<usize>,
        render_topo: bool,
        no_export: bool,
        override_antenna_mhz: Option<f32>,
        override_antenna_separation: Option<f32>,
        override_date: Option<Py<PyAny>>,
        lines: Option<Py<PyAny>>,
        metadata: Option<Py<PyAny>>,
        radargram_id: Option<String>,
        radargram_name: Option<String>,
        group_id: Option<String>,
        group_name: Option<String>,
        return_dataset_format: &str,
    ) -> PyResult<Py<PyAny>> {
        use pyo3::exceptions::PyValueError;

        if !["xarray", "xarray_dict"].contains(&return_dataset_format) {
            return Err(PyNotImplementedError::new_err(
                "Only 'xarray_dict' and 'xarray' return formats are supported for now",
            ));
        };

        let track_path = path_or_flag(py, track)?;
        let render_path = path_or_flag(py, render)?;

        if return_dataset {
            if output.is_some() {
                return Err(PyValueError::new_err(
                    "return_dataset=True requires output=None",
                ));
            }
            if render_path.is_some() {
                return Err(PyValueError::new_err(
                    "return_dataset=True is incompatible with render=...",
                ));
            }
            if track_path.is_some() {
                return Err(PyValueError::new_err(
                    "return_dataset=True is incompatible with track=...",
                ));
            }
        }
        let options = processing_options(
            py,
            steps,
            default,
            default_with_topo,
            velocity,
            cor,
            dem,
            crs,
            quiet,
            render_profile,
            render_width,
            render_topo,
            // A returned dataset is built but not written.
            no_export || return_dataset,
            override_antenna_mhz,
            override_antenna_separation,
            override_date,
            lines,
            metadata,
            group_name,
            group_id,
        )?;
        let params = crate::processing::RunParams {
            filepaths: inputs_to_paths(py, inputs.bind(py))?,
            output_path: optional_path(py, output)?,
            track_path,
            render_path,
            radargram_id,
            display_name: radargram_name,
            options,
        };

        if return_dataset {
            let (gpr_obj, _default_path) = crate::processing::build_processed_gpr(params)
                .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(format!("{e:?}")))?;
            let ds_py = gpr_obj
                .export_dataset()
                .map_err(pyo3::exceptions::PyRuntimeError::new_err)?
                .to_python(py);
            return match return_dataset_format {
                "xarray" => xarray_dict_to_ds(py, ds_py?),
                _ => ds_py,
            };
        }

        let result = crate::processing::run(params)
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(format!("{e:?}")))?;
        // Ok(result.output_path.to_string_lossy().to_string())
        Ok(py
            .eval(c_str!("str"), None, None)?
            .call1((result.output_path.to_string_lossy().to_string(),))?
            .unbind())
    }
    /// Read one or more GPR files into memory without applying a processing workflow.
    ///
    /// Use this function to load radar data in a lightweight form for inspection,
    /// exploration, or downstream processing in Python. Unlike `process()`,
    /// `read()` is intended to return the data essentially as read from disk rather
    /// than applying a full processing workflow.
    ///
    /// Parameters
    /// ----------
    /// inputs : path-like or sequence of path-like
    ///     One or more input files to read. A single path, list, or tuple of
    ///     path-like objects is accepted.
    /// velocity : float, default 0.168
    ///     Propagation velocity in meters per nanosecond.
    /// cor : path-like, optional
    ///     Positions to use instead of the recording's own. A file (the
    ///     format's own positions file, such as a ``.cor``, or a Ridal
    ///     coordinate CSV of ``trace,time,lon,lat[,alt]``) for one recording
    ///     or the lines of one IceRadar file, or a directory holding ``<stem>.coords.csv`` or the format's own
    ///     positions file for each. Without it, a ``<stem>.coords.csv``
    ///     beside a recording is used before its own positions file.
    /// dem : path-like, optional
    ///     Digital elevation model to sample for topographic information.
    /// crs : str, optional
    ///     Coordinate reference system for interpreting or transforming coordinates.
    ///     If omitted, the most appropriate WGS84 UTM zone is used.
    /// override_antenna_mhz : float, optional
    ///     Override the antenna center frequency inferred from the input data.
    /// override_antenna_separation : float, optional
    ///     Override the antenna separation inferred from the input data.
    /// override_date : datetime.date or str, optional
    ///     The UTC date of recordings whose files record only a time of day,
    ///     or a date that may be wrong: IceRadar, whose date is the
    ///     acquisition PC's clock, and pulseEKKO. A ``datetime.date`` or a
    ///     ``"YYYY-MM-DD"`` string; a ``datetime.datetime`` is refused.
    /// lines : int, sequence of int or str, optional
    ///     Which lines of a file holding several (IceRadar) to read: one
    ///     (``3``), several merged in the order given (``[0, 2, 3]`` or
    ///     ``"0,2-3"``), or ``"all"``. Lines recorded with different settings
    ///     cannot be merged. Required for a file of more than one line, and
    ///     refused for formats without lines.
    /// metadata : mapping, optional
    ///     Additional user metadata to attach to the returned dataset. This should
    ///     be a JSON-serializable mapping. Root keys are interpreted as strings.
    /// return_dataset_format : str, default "xarray_dict"
    ///     Format of the returned in-memory dataset.
    ///
    ///     Supported values currently include:
    ///
    ///     - ``"xarray_dict"`` for a plain Python representation that does not
    ///       require importing `xarray`.
    ///     - ``"xarray"`` for an `xarray.Dataset`, which requires `xarray` to be
    ///       installed.
    ///
    ///     More return formats may be added in the future.
    ///
    /// Returns
    /// -------
    /// object
    ///     An in-memory dataset representation of the input data.
    ///
    /// Raises
    /// ------
    /// RuntimeError
    ///     If reading fails.
    /// NotImplementedError
    ///     If `return_dataset_format` is not supported.
    ///
    /// Notes
    /// -----
    /// `read()` is intended as a lightweight loader. If you want to apply filtering,
    /// corrections, or export a processed dataset, use `process()` instead.
    #[pyfunction]
    #[allow(clippy::too_many_arguments)]
    #[pyo3(signature = (
        inputs,
        *,
        velocity=0.168,
        cor=None,
        dem=None,
        crs=None,
        override_antenna_mhz=None,
        override_antenna_separation=None,
        override_date=None,
        lines=None,
        metadata=None,
        return_dataset_format="xarray_dict"
    ))]
    fn read(
        py: Python<'_>,
        inputs: Py<PyAny>,
        velocity: f32,
        cor: Option<Py<PyAny>>,
        dem: Option<Py<PyAny>>,
        crs: Option<String>,
        override_antenna_mhz: Option<f32>,
        override_antenna_separation: Option<f32>,
        override_date: Option<Py<PyAny>>,
        lines: Option<Py<PyAny>>,
        metadata: Option<Py<PyAny>>,
        return_dataset_format: &str,
    ) -> PyResult<Py<PyAny>> {
        process(
            py,
            inputs,
            None,
            None,
            true,
            false,
            false,
            velocity,
            cor,
            dem,
            crs,
            None,
            true,
            None,
            None,
            None,
            false,
            false,
            override_antenna_mhz,
            override_antenna_separation,
            override_date,
            lines,
            metadata,
            None,
            None,
            None,
            None,
            return_dataset_format,
        )
    }
    /// Batch-process one or more GPR files into multiple outputs.
    ///
    /// Use this function when many input files should be processed in one call and
    /// written as separate outputs in an existing output directory.
    ///
    /// Parameters
    /// ----------
    /// inputs : path-like or sequence of path-like
    ///     One or more input files to process. A single path, list, or tuple of
    ///     path-like objects is accepted.
    /// output : path-like
    ///     Existing output directory where processed datasets will be written.
    /// steps : str or sequence of str, optional
    ///     Processing steps to apply. This may be given either as a comma-separated
    ///     string or as a sequence of step names.
    ///
    ///     Available steps are exposed as `ridal.all_steps`, and descriptions are
    ///     available in `ridal.all_step_descriptions`.
    ///
    ///     Exactly one of `steps`, `default`, and `default_with_topo` may be given.
    /// default : bool, default False
    ///     Use the default processing profile.
    ///
    ///     Exactly one of `steps`, `default`, and `default_with_topo` may be given.
    /// default_with_topo : bool, default False
    ///     Use the default processing profile and include topographic correction.
    ///
    ///     Exactly one of `steps`, `default`, and `default_with_topo` may be given.
    /// velocity : float, default 0.168
    ///     Propagation velocity in meters per nanosecond.
    /// cor : path-like, optional
    ///     Positions to use instead of the recording's own. A file (the
    ///     format's own positions file, such as a ``.cor``, or a Ridal
    ///     coordinate CSV of ``trace,time,lon,lat[,alt]``) for one recording
    ///     or the lines of one IceRadar file, or a directory holding ``<stem>.coords.csv`` or the format's own
    ///     positions file for each. Without it, a ``<stem>.coords.csv``
    ///     beside a recording is used before its own positions file.
    /// dem : path-like, optional
    ///     Digital elevation model to sample for topographic information.
    /// crs : str, optional
    ///     Coordinate reference system for interpreting or transforming coordinates.
    ///     If omitted, the most appropriate WGS84 UTM zone is used.
    /// track : path-like, optional
    ///     Existing directory where exported track files should be written.
    /// quiet : bool, default False
    ///     Reduce logging and progress output.
    /// render : path-like, optional
    ///     Existing directory where rendered figures should be written.
    /// render_profile : str, optional
    ///     Render profile for `render`: a built-in name, or a path to a TOML
    ///     file describing one. Defaults to the built-in `"default"`. Distinct
    ///     from the *processing* profile selected by `default=` and `steps=`.
    /// render_width : int, optional
    ///     Output width in pixels for `render`. Defaults to one pixel per
    ///     trace; a width beyond the trace count is clamped, not upsampled to.
    /// render_topo : bool, default False
    ///     Render the topographically corrected view for `render`, as in
    ///     `process()`.
    /// no_export : bool, default False
    ///     Run processing without writing the main dataset outputs. Side outputs
    ///     such as rendered figures or exported tracks may still be produced.
    /// merge : str, optional
    ///     Merge chronologically neighboring profiles when they are close enough in
    ///     time and otherwise compatible.
    ///
    ///     For example, ``"10 min"`` will merge neighboring profiles separated by
    ///     less than ten minutes.
    ///
    ///     The value is parsed using the `parse_duration` syntax. Briefly, it
    ///     accepts sequences of ``[value] [unit]`` pairs such as
    ///     ``"15 days 20 seconds 100 milliseconds"``; spaces are optional, and
    ///     unit order does not matter. See the full syntax and accepted
    ///     abbreviations at:
    ///     https://docs.rs/parse_duration/latest/parse_duration/#syntax
    /// override_antenna_mhz : float, optional
    ///     Override the antenna center frequency inferred from the input data.
    /// override_antenna_separation : float, optional
    ///     Override the antenna separation inferred from the input data.
    /// override_date : datetime.date or str, optional
    ///     The UTC date of recordings whose files record only a time of day,
    ///     or a date that may be wrong: IceRadar, whose date is the
    ///     acquisition PC's clock, and pulseEKKO. A ``datetime.date`` or a
    ///     ``"YYYY-MM-DD"`` string; a ``datetime.datetime`` is refused.
    /// metadata : mapping, optional
    ///     Additional user metadata to attach independently to each produced output.
    ///     This should be a JSON-serializable mapping. Root keys are interpreted as
    ///     strings.
    /// group_id : str, optional
    ///     Explicit identifier for the group all outputs in this batch belong to,
    ///     overriding the id derived from `group_name`.
    /// group_name : str, optional
    ///     Human-readable name of the group all outputs in this batch belong to
    ///     (survey, campaign, location), used for catalog grouping. Non-ASCII
    ///     is accepted and written unchanged; see `process` for the reader
    ///     caveat. Applied uniformly to the whole batch; radargram IDs and
    ///     display names are still derived per output, unless `id_pattern` and
    ///     `display_name_pattern` say otherwise.
    /// id_pattern : str, optional
    ///     Name each output's radargram id after a pattern, and its file
    ///     ``<id>.nc``. Placeholders: ``{stem}`` (the first input's file stem),
    ///     ``{n}`` (a counter over the outputs in time order), ``{freq}``
    ///     (antenna frequency in MHz), ``{nfiles}`` (how many inputs were
    ///     merged), ``{date}`` (``YYYYMMDD``) and ``{group}``. ``{n:02}`` pads to
    ///     two digits; ``{n, from=15}`` starts at 15, ``{n, from=next}``
    ///     continues after the ids already in `output`, and ``{n, per=freq}``
    ///     counts each frequency separately. For example,
    ///     ``"dronbreen-2026-{freq}mhz-{n:02, per=freq}"``. Literal text may only
    ///     hold lowercase ASCII letters, digits, ``-`` and ``_``. Every id is
    ///     checked before anything is processed, and two outputs with the same
    ///     id are an error. Without a pattern, each id comes from its first
    ///     input's file name.
    /// display_name_pattern : str, optional
    ///     Name each output's display name after a pattern: the placeholders of
    ///     `id_pattern`, plus ``{id}`` and ``{group_name}``, and any text. A
    ///     bare ``{n}`` is the number in the output's id. Needs `id_pattern`.
    ///
    /// Returns
    /// -------
    /// list of str
    ///     Output dataset paths as strings, in the order produced.
    ///
    /// Raises
    /// ------
    /// ValueError
    ///     If incompatible arguments are provided, including:
    ///
    ///     - more than one of `steps`, `default`, and `default_with_topo`
    ///     - `output` is not an existing directory
    ///     - `track` is provided but is not an existing directory
    ///     - `render` is provided but is not an existing directory
    ///     - an invalid `steps` value
    /// RuntimeError
    ///     If batch processing fails.
    ///
    /// Notes
    /// -----
    /// Unlike `process()`, `batch_process()` always targets an existing output
    /// directory and produces multiple outputs.
    #[pyfunction]
    #[allow(clippy::too_many_arguments)]
    #[pyo3(signature = (
    inputs,
    output,
    *,
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
    render_profile=None,
    render_width=None,
    render_topo=false,
        no_export=false,
        merge=None,
        override_antenna_mhz=None,
        override_antenna_separation=None,
        override_date=None,
        metadata=None,
        group_id=None,
        group_name=None,
        id_pattern=None,
        display_name_pattern=None,
 ))]
    fn batch_process(
        py: Python<'_>,
        inputs: Py<PyAny>,
        output: Py<PyAny>,
        steps: Option<Py<PyAny>>,
        default: bool,
        default_with_topo: bool,
        velocity: f32,
        cor: Option<Py<PyAny>>,
        dem: Option<Py<PyAny>>,
        crs: Option<String>,
        track: Option<Py<PyAny>>,
        quiet: bool,
        render: Option<Py<PyAny>>,
        render_profile: Option<String>,
        render_width: Option<usize>,
        render_topo: bool,
        no_export: bool,
        merge: Option<String>,
        override_antenna_mhz: Option<f32>,
        override_antenna_separation: Option<f32>,
        override_date: Option<Py<PyAny>>,
        metadata: Option<Py<PyAny>>,
        group_id: Option<String>,
        group_name: Option<String>,
        id_pattern: Option<String>,
        display_name_pattern: Option<String>,
    ) -> PyResult<Vec<String>> {
        use pyo3::exceptions::{PyRuntimeError, PyValueError};

        let output_dir = fspath(py, output.bind(py))?;
        if !output_dir.is_dir() {
            return Err(PyValueError::new_err(format!(
                "output must be an existing directory in batch_process(): {}",
                output_dir.display()
            )));
        }
        let params = crate::processing::batch::BatchRunParams {
            filepaths: inputs_to_paths(py, inputs.bind(py))?,
            output_dir,
            track_dir: existing_dir(py, track, "track")?,
            render_dir: existing_dir(py, render, "render")?,
            merge,
            id_pattern,
            display_name_pattern,
            options: processing_options(
                py,
                steps,
                default,
                default_with_topo,
                velocity,
                cor,
                dem,
                crs,
                quiet,
                render_profile,
                render_width,
                render_topo,
                no_export,
                override_antenna_mhz,
                override_antenna_separation,
                override_date,
                // Batch processing does not split a file into lines yet.
                None,
                metadata,
                group_name,
                group_id,
            )?,
        };

        let result = crate::processing::batch::run_batch(params)
            .map_err(|e| PyRuntimeError::new_err(format!("{e:?}")))?;

        Ok(result
            .output_paths
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect())
    }

    /// Render a processed radargram to an image file.
    ///
    /// The Python equivalent of `ridal render`, and the same renderer the web
    /// GUI uses, so an image produced here matches one downloaded from the
    /// browser for the same file and profile.
    ///
    /// Parameters
    /// ----------
    /// input : path-like
    ///     Processed `.nc` file to render.
    /// output : path-like, optional
    ///     Output image path. The extension selects the encoding (`.png` or
    ///     `.jpg`). If omitted, a sidecar beside the input is written, using
    ///     the profile's own format.
    /// profile : str, default "default"
    ///     A built-in render profile name, or a path to a TOML file describing
    ///     one. Built-in names win, so a file of the same name in the working
    ///     directory cannot shadow them.
    /// width : int, optional
    ///     Output width in pixels. Defaults to one pixel per trace. A width
    ///     beyond the trace count is clamped rather than upsampled to.
    /// quality : int, optional
    ///     JPEG quality, 1-100. Ignored when the output is PNG.
    /// topo : bool, default False
    ///     Render the topographically corrected view, as `ridal render --topo`
    ///     does, rather than the standard one. Needs the file's `elevation`
    ///     and `depth` axes; without usable ones this raises rather than
    ///     falling back to a standard render.
    /// figure : dict or path-like, optional
    ///     Render an annotated figure instead: an excerpt with axes, ticks and
    ///     labels, as `ridal render --figure` does. A figure spec, either as a
    ///     dict or as the path to a JSON file holding one; see the rendering
    ///     guide for its fields. The output's extension (`.svg` or `.png`)
    ///     picks the format, and defaults to `<input stem>.figure.png`. The
    ///     spec sets the view, extent, size and profile, so `profile`,
    ///     `width`, `quality` and `topo` cannot be given with it. A spec with
    ///     `"axes": false` also writes a `.json` sidecar beside the image.
    /// project : path-like, optional
    ///     With `figure`: the project directory the radargram belongs to. A
    ///     figure's picks and derived layers are read from it, all of them,
    ///     and the topographic view uses the radargram's elevation window
    ///     from it. Needed when the spec asks for picks or derived layers.
    ///
    /// Returns
    /// -------
    /// dict
    ///     `{"path": str, "width": int, "height": int, "profile": str}`. A
    ///     figure also has `"warnings"`, a list of what could not be drawn
    ///     as stored (picks that could not be carried to this revision), and
    ///     one without axes `"sidecar"`, the sidecar's path.
    ///
    /// Raises
    /// ------
    /// RuntimeError
    ///     If the input cannot be read, the profile cannot be resolved, the
    ///     image cannot be written, `topo=True` and the file cannot be
    ///     topographically corrected, or the figure spec is invalid or does
    ///     not suit the file.
    /// ValueError
    ///     If `figure` is given together with `profile`, `width`, `quality` or
    ///     `topo`, or `project` without `figure`.
    ///
    /// Notes
    /// -----
    /// To render while processing raw data instead, pass `render=` to
    /// `process()` or `batch_process()`; those accept the same `render_profile`,
    /// `render_width` and `render_topo` and go through this same pipeline.
    #[pyfunction]
    #[pyo3(signature = (input, output=None, *, profile=None, width=None, quality=None, topo=false, figure=None, project=None))]
    #[allow(clippy::too_many_arguments)]
    fn render(
        py: Python<'_>,
        input: Py<PyAny>,
        output: Option<Py<PyAny>>,
        profile: Option<String>,
        width: Option<usize>,
        quality: Option<u8>,
        topo: bool,
        figure: Option<Py<PyAny>>,
        project: Option<Py<PyAny>>,
    ) -> PyResult<Py<PyAny>> {
        use pyo3::exceptions::{PyRuntimeError, PyValueError};

        let input = fspath(py, input.bind(py))?;
        if figure.is_none() && project.is_some() {
            return Err(PyValueError::new_err(
                "project= is where a figure's picks and derived layers come from; it needs \
                 figure=",
            ));
        }
        if let Some(figure) = figure {
            if profile.is_some() || width.is_some() || quality.is_some() || topo {
                return Err(PyValueError::new_err(
                    "figure= sets the view, size and profile itself; do not also pass \
                     profile, width, quality or topo",
                ));
            }
            let figure = figure.bind(py);
            let spec = if figure.is_instance_of::<pyo3::types::PyDict>() {
                let text: String = py
                    .import("json")?
                    .call_method1("dumps", (figure,))?
                    .extract()?;
                crate::render::figure::spec::FigureSpec::from_json(&text)
                    .map_err(PyRuntimeError::new_err)?
            } else {
                crate::render::figure::read_spec(&fspath(py, figure)?)
                    .map_err(PyRuntimeError::new_err)?
            };
            let output = match output {
                Some(value) => fspath(py, value.bind(py))?,
                None => {
                    let stem = input.file_stem().unwrap_or_default().to_string_lossy();
                    input.with_file_name(format!("{stem}.figure.png"))
                }
            };
            let project = project.map(|p| fspath(py, p.bind(py))).transpose()?;
            let written = crate::render::figure::render_figure_path(
                &input,
                &output,
                &spec,
                project.as_deref(),
            )
            .map_err(PyRuntimeError::new_err)?;
            let out = pyo3::types::PyDict::new(py);
            out.set_item("path", output.display().to_string())?;
            out.set_item("width", written.width)?;
            out.set_item("height", written.height)?;
            out.set_item("profile", &spec.profile)?;
            if let Some(sidecar) = &written.sidecar {
                out.set_item("sidecar", sidecar.display().to_string())?;
            }
            out.set_item("warnings", written.warnings)?;
            return Ok(out.into_any().unbind());
        }
        let resolved =
            crate::render::profile::RenderProfile::resolve(profile.as_deref().unwrap_or("default"))
                .map_err(PyRuntimeError::new_err)?;
        let output = match output {
            Some(value) => fspath(py, value.bind(py))?,
            None => crate::render::oneshot::sidecar_path(&input, &resolved),
        };

        let request = crate::render::oneshot::RenderRequest {
            profile: &resolved,
            width,
            quality,
        };
        let rendered = if topo {
            crate::render::oneshot::render_topo_path_to_file(&input, &output, &request)
        } else {
            crate::render::oneshot::render_path_to_file(&input, &output, &request)
        };
        let (w, h) = rendered.map_err(PyRuntimeError::new_err)?;

        let out = pyo3::types::PyDict::new(py);
        out.set_item("path", output.display().to_string())?;
        out.set_item("width", w)?;
        out.set_item("height", h)?;
        out.set_item("profile", resolved.name)?;
        Ok(out.into_any().unbind())
    }

    /// Write a recording's positions as a coordinate CSV.
    ///
    /// The CSV has one row per trace, and ``process()`` reads it in place of
    /// the recording's own positions when it is ``<stem>.coords.csv`` beside
    /// the recording (the default here) or given as ``cor``. Positions come
    /// from ``positions``, from ``start`` and ``end``, from a GNSS ``track``,
    /// or from the recording's own positions file, and are interpolated
    /// between. Times come from the given positions, else the recording, else
    /// they are made up from ``start_time`` and ``time_interval`` and marked
    /// synthetic. Altitudes come from ``dem`` first. Every row says where its
    /// time and altitude came from. This is ``ridal coords`` on the command
    /// line.
    ///
    /// Parameters
    /// ----------
    /// input : path-like
    ///     Any one of the recording's files.
    /// output : path-like, optional
    ///     The CSV to write. Defaults to ``<stem>.coords.csv`` beside the
    ///     recording.
    /// positions : path-like, optional
    ///     A CSV of known positions: ``trace`` (0-based) and ``lon,lat`` (or
    ///     ``x,y`` with ``src_crs``), and optionally ``alt`` and ``time``.
    /// start, end : tuple of float, optional
    ///     The first and last trace's positions, ``(lon, lat)`` or
    ///     ``(lon, lat, alt)`` (``x, y`` with ``src_crs``). Give both or
    ///     neither.
    /// track : path-like, optional
    ///     A GNSS track: ``time`` and ``lon,lat`` (or ``x,y``), and optionally
    ///     ``alt``. Each trace gets the position at its time on the
    ///     recording's clock.
    /// time_offset : float, optional
    ///     Seconds to add to the recording's clock to get the track's.
    /// src_crs : str, optional
    ///     The CRS of ``x,y`` positions. Without it, positions are WGS84
    ///     longitude and latitude.
    /// start_time : str or datetime.datetime, optional
    ///     When the first trace was recorded. Makes the times up from it and
    ///     the trace interval. A time without a UTC offset is taken as UTC.
    /// time_interval : float, optional
    ///     Seconds between traces for made-up times. Defaults to the header's.
    /// dem : path-like, optional
    ///     A DEM to take every altitude from, before any other.
    /// overwrite : bool, default False
    ///     Replace ``output`` if it exists.
    ///
    /// Returns
    /// -------
    /// str
    ///     The path of the CSV.
    ///
    /// Warns
    /// -----
    /// UserWarning
    ///     For what a person should know about the result: times made up from
    ///     1970-01-01, an interval taken from the header, traces outside a
    ///     track, or no altitudes.
    ///
    /// Raises
    /// ------
    /// RuntimeError
    ///     If the positions cannot be built: nothing gives them, a file is
    ///     malformed, a track's times do not overlap the recording's, or
    ///     ``output`` exists without ``overwrite``.
    #[pyfunction]
    #[allow(clippy::too_many_arguments)]
    #[pyo3(signature = (
        input,
        output=None,
        *,
        positions=None,
        start=None,
        end=None,
        track=None,
        time_offset=None,
        src_crs=None,
        start_time=None,
        time_interval=None,
        dem=None,
        overwrite=false,
    ))]
    fn coords(
        py: Python<'_>,
        input: Py<PyAny>,
        output: Option<Py<PyAny>>,
        positions: Option<Py<PyAny>>,
        start: Option<Vec<f64>>,
        end: Option<Vec<f64>>,
        track: Option<Py<PyAny>>,
        time_offset: Option<f64>,
        src_crs: Option<String>,
        start_time: Option<Py<PyAny>>,
        time_interval: Option<f64>,
        dem: Option<Py<PyAny>>,
        overwrite: bool,
    ) -> PyResult<String> {
        let point = |values: Vec<f64>, name: &str| match values[..] {
            [a, b] => Ok(crate::positions::Point { a, b, alt: None }),
            [a, b, alt] => Ok(crate::positions::Point {
                a,
                b,
                alt: Some(alt),
            }),
            _ => Err(pyo3::exceptions::PyValueError::new_err(format!(
                "{name} is (lon, lat) or (lon, lat, alt), not {} numbers",
                values.len()
            ))),
        };
        let endpoints = match (start, end) {
            (Some(start), Some(end)) => Some((point(start, "start")?, point(end, "end")?)),
            (None, None) => None,
            _ => {
                return Err(pyo3::exceptions::PyValueError::new_err(
                    "give both start and end, or neither",
                ))
            }
        };
        // A datetime is given as its ISO 8601 text, which keeps its offset
        // if it has one.
        let start_time = start_time
            .map(|value| {
                let value = value.bind(py);
                if value.hasattr("isoformat")? {
                    value.call_method0("isoformat")?.extract::<String>()
                } else {
                    value.extract::<String>()
                }
            })
            .transpose()?;
        let recording = fspath(py, input.bind(py))?;
        let output =
            crate::positions::output_path(&recording, optional_path(py, output)?.as_deref(), None)
                .map_err(PyRuntimeError::new_err)?;
        let request = crate::positions::Request {
            recording,
            positions: optional_path(py, positions)?,
            endpoints,
            track: optional_path(py, track)?,
            time_offset,
            src_crs,
            start_time,
            time_interval,
            dem: optional_path(py, dem)?,
        };
        let built = crate::positions::write(&request, &output, overwrite)
            .map_err(PyRuntimeError::new_err)?;
        let warn = py.import("warnings")?.getattr("warn")?;
        for warning in built.warnings {
            warn.call1((warning,))?;
        }
        Ok(output.display().to_string())
    }

    /// Inspect one or more GPR files and return metadata summaries.
    ///
    /// This function reads metadata and summary information without performing a
    /// full processing workflow. Raw recordings and NetCDF files Ridal processed
    /// can both be inspected, and mixed.
    ///
    /// Parameters
    /// ----------
    /// inputs : path-like or sequence of path-like
    ///     One or more input files to inspect. A single path, list, or tuple of
    ///     path-like objects is accepted. A ``.nc`` file is read as Ridal output.
    /// velocity : float, default 0.168
    ///     Propagation velocity in meters per nanosecond.
    /// cor : path-like, optional
    ///     Positions to use instead of the recording's own. A file (the
    ///     format's own positions file, such as a ``.cor``, or a Ridal
    ///     coordinate CSV of ``trace,time,lon,lat[,alt]``) for one recording
    ///     or the lines of one IceRadar file, or a directory holding ``<stem>.coords.csv`` or the format's own
    ///     positions file for each. Without it, a ``<stem>.coords.csv``
    ///     beside a recording is used before its own positions file.
    /// dem : path-like, optional
    ///     Digital elevation model to sample for topographic information.
    /// crs : str, optional
    ///     Coordinate reference system for interpreting or transforming coordinates.
    ///     If omitted, the most appropriate WGS84 UTM zone is used.
    /// override_antenna_mhz : float, optional
    ///     Override the antenna center frequency inferred from the input data.
    /// override_antenna_separation : float, optional
    ///     Override the antenna separation inferred from the input data.
    /// override_date : datetime.date or str, optional
    ///     The UTC date of recordings whose files record only a time of day,
    ///     or a date that may be wrong: IceRadar, whose date is the
    ///     acquisition PC's clock, and pulseEKKO. A ``datetime.date`` or a
    ///     ``"YYYY-MM-DD"`` string; a ``datetime.datetime`` is refused.
    ///
    /// Returns
    /// -------
    /// list of dict
    ///     One metadata summary dictionary per input file. ``record["format"]["name"]``
    ///     tells the two kinds apart: for a processed file it is ``"ridal"``, and
    ///     the record holds ``ridal_version``, ``radargram_id``, ``display_name``,
    ///     ``group_name``, ``group_id``, ``processing_datetime``, ``recipe_hash``,
    ///     ``revision_id``, ``samples`` and ``traces``. ``revision_id`` is the id a
    ///     Ridal server gives this revision of the radargram, so a local file can be
    ///     matched to one a server already has. It is derived from ``recipe_hash``,
    ///     the hash of the raw files and processing the file was made from, so the
    ///     same raw data processed the same way has the same ``revision_id``. A file
    ///     written before recipes were recorded has ``recipe_hash`` ``None``, and
    ///     its ``revision_id`` comes from ``processing_datetime`` instead. A file from a Ridal too old to have radargram
    ///     ids has ``reprocess_reason`` set and the identity fields ``None``; for
    ///     every other file ``reprocess_reason`` is ``None``.
    ///
    /// Raises
    /// ------
    /// RuntimeError
    ///     If inspection fails, if a ``.nc`` file is not one Ridal processed, or
    ///     if ``cor``, ``dem``, ``crs`` or an override is given with a processed
    ///     file, which they do not apply to.
    ///
    /// Notes
    /// -----
    /// `info()` is intended for lightweight inspection. For loading in-memory data,
    /// use `read()`. For modifying or exporting processed data, use `process()` or
    /// `batch_process()`.
    #[pyfunction]
    #[allow(clippy::too_many_arguments)]
    #[pyo3(signature = (
        inputs,
        *,
        velocity=0.168,
        cor=None,
        dem=None,
        crs=None,
        override_antenna_mhz=None,
        override_antenna_separation=None,
        override_date=None,
    ))]
    fn info(
        py: Python<'_>,
        inputs: Py<PyAny>,
        velocity: f32,
        cor: Option<Py<PyAny>>,
        dem: Option<Py<PyAny>>,
        crs: Option<String>,
        override_antenna_mhz: Option<f32>,
        override_antenna_separation: Option<f32>,
        override_date: Option<Py<PyAny>>,
    ) -> PyResult<Vec<Py<PyAny>>> {
        let input_paths = inputs_to_paths(py, inputs.bind(py))?;
        let cor_path = optional_path(py, cor)?;
        let dem_path = optional_path(py, dem)?;

        let params = crate::inspect::InfoParams {
            filepaths: input_paths,
            dem_path,
            cor_path,
            medium_velocity: velocity,
            crs,
            override_antenna_mhz,
            override_antenna_separation,
            override_date: date_arg(py, override_date)?,
        };
        let records = crate::inspect::inspect(params)
            .map_err(|e| PyRuntimeError::new_err(format!("{e:?}")))?;

        records
            .iter()
            .map(|r| {
                let text = serde_json::to_string(r).map_err(|e| {
                    PyRuntimeError::new_err(format!("Failed to serialize info record to JSON: {e}"))
                })?;
                json_to_py(py, &text)
            })
            .collect::<PyResult<Vec<Py<PyAny>>>>()
    }

    /// The body of a replace preflight for a processed file (#331). Private.
    ///
    /// For the HTTP client (#328): the file's radargram id, processing
    /// datetime and axis declarations, as ``POST
    /// /api/v1/datasets/{radargram_id}/replace/preflight`` takes them. The
    /// shape is the server's request contract (``RevisionDeclarations`` in
    /// the OpenAPI description) and changes with it.
    ///
    /// Raises
    /// ------
    /// RuntimeError
    ///     If the file cannot be read, or is not a current Ridal file.
    #[pyfunction]
    fn _preflight_body(py: Python<'_>, path: Py<PyAny>) -> PyResult<Py<PyAny>> {
        let path = fspath(py, path.bind(py))?;
        let body = crate::interp::source::RevisionDeclarations::read(&path)
            .map_err(PyRuntimeError::new_err)?;
        let text = serde_json::to_string(&body).map_err(|e| {
            PyRuntimeError::new_err(format!("Failed to serialize the preflight body: {e}"))
        })?;
        json_to_py(py, &text)
    }

    /// `value` as a one-dimensional float array, as a `Vec`.
    fn f64_vector(py: Python<'_>, value: &Bound<'_, PyAny>, name: &str) -> PyResult<Vec<f64>> {
        let numpy = py.import("numpy")?;
        let kwargs = PyDict::new(py);
        kwargs.set_item("dtype", numpy.getattr("float64")?)?;
        let array = numpy.getattr("asarray")?.call((value,), Some(&kwargs))?;
        let array = array.cast_into::<numpy::PyArray1<f64>>().map_err(|_| {
            pyo3::exceptions::PyValueError::new_err(format!(
                "{name} must be one-dimensional, one value per {}",
                if name == "twtt_ns" { "sample" } else { "trace" }
            ))
        })?;
        Ok(array.readonly().as_array().to_vec())
    }

    /// Trace times as seconds since the Unix epoch, from `datetime64` or
    /// from numbers that already are; `NaT` becomes NaN, which the
    /// validation then names.
    fn epoch_seconds(py: Python<'_>, value: &Bound<'_, PyAny>) -> PyResult<Vec<f64>> {
        let numpy = py.import("numpy")?;
        let array = numpy.getattr("asarray")?.call1((value,))?;
        let kind: String = array.getattr("dtype")?.getattr("kind")?.extract()?;
        let seconds = if kind == "M" {
            let epoch = numpy.getattr("datetime64")?.call1((0, "s"))?;
            let second = numpy.getattr("timedelta64")?.call1((1, "s"))?;
            array.sub(epoch)?.div(second)?
        } else {
            array
        };
        f64_vector(py, &seconds, "trace_time")
    }

    /// Write a radargram processed elsewhere as a Ridal NetCDF file.
    ///
    /// Use this to bring data from another tool (or a script) into Ridal,
    /// for example to pick it in the web interface: the file has the layout
    /// `process()` writes and goes through the same export. Nothing is
    /// processed. :func:`ridal.impdar.from_radardata` does this for an
    /// ImpDAR ``RadarData``.
    ///
    /// Every per-trace value is required for every trace, and must be
    /// finite; an error names the traces that are not.
    ///
    /// Parameters
    /// ----------
    /// path : path-like
    ///     The ``.nc`` file to write. An existing file is replaced.
    /// data : array_like
    ///     Amplitudes, shaped ``(samples, traces)``.
    /// twtt_ns : array_like
    ///     Time of each sample in nanoseconds, evenly spaced. Travel time
    ///     from time zero when `time_zero_ns` is given; time since the
    ///     instrument started sampling when it is None.
    /// time_zero_ns : float, array_like or None
    ///     Where time zero (the direct wave's arrival) sits on the recording
    ///     clock, in nanoseconds: how far into the original record it is. One
    ///     value, or one per trace. None when it has never been located; no
    ///     value is assumed. Required, so that this is always a decision.
    ///     Picks are carried between two versions of a radargram through
    ///     this and `twtt_ns`, so it needs to be right for that to work.
    /// trace_time : array_like
    ///     Time of each trace, as ``datetime64`` or seconds since the Unix
    ///     epoch (UTC).
    /// longitude, latitude : array_like, optional
    ///     WGS84 position of each trace, in degrees. Give these, or `x` and
    ///     `y`, not both.
    /// x, y : array_like, optional
    ///     Projected position of each trace, in the units of `crs`.
    /// crs : str, optional
    ///     Coordinate reference system of `x` and `y` (required with them),
    ///     or the one to project `longitude` and `latitude` into (default:
    ///     the best WGS84 UTM zone).
    /// elevation : array_like, optional
    ///     Elevation of each trace, in metres. Give this or `dem`.
    /// dem : path-like, optional
    ///     Digital elevation model to sample each trace's elevation from.
    /// antenna_mhz : float
    ///     Antenna centre frequency in MHz.
    /// antenna_separation : float
    ///     Transmitter-receiver separation at acquisition, in metres.
    /// antenna_separation_effective : float, optional
    ///     The separation the data still needs correcting for: equal to
    ///     `antenna_separation` (the default) when nothing corrected for it,
    ///     ``0`` after a normal-moveout (NMO) correction.
    /// medium_velocity : float, default 0.168
    ///     Radar wave velocity in the medium, in m/ns, used for depths.
    /// radargram_id : str, optional
    ///     Stable, unique identifier for the radargram (lowercase ASCII,
    ///     digits, ``-``, ``_``). Defaults to the file stem, with a warning.
    ///     Give every version of one radargram the same id, so picks drawn
    ///     on one can be carried to the next.
    /// radargram_name, group_id, group_name : str, optional
    ///     As in `process()`.
    /// metadata : mapping, optional
    ///     Additional JSON-serializable user metadata, as in `process()`.
    /// log : sequence of str, optional
    ///     How the data was processed, one line each, written to the file's
    ///     processing log ahead of Ridal's own line.
    /// source_file : str, optional
    ///     The file the data was made from, recorded in
    ///     ``original_filepaths``.
    /// antenna : str, optional
    ///     The antenna's name. Defaults to its frequency.
    ///
    /// Returns
    /// -------
    /// str
    ///     The path written.
    ///
    /// Raises
    /// ------
    /// ValueError
    ///     If an argument is missing, the wrong shape or length, not finite,
    ///     out of range, or inconsistent with another.
    /// RuntimeError
    ///     If the DEM cannot be sampled or the file cannot be written.
    #[pyfunction]
    #[allow(clippy::too_many_arguments)]
    #[pyo3(signature = (
        path,
        data,
        *,
        twtt_ns,
        time_zero_ns,
        trace_time,
        antenna_mhz,
        antenna_separation,
        longitude=None,
        latitude=None,
        x=None,
        y=None,
        crs=None,
        elevation=None,
        dem=None,
        antenna_separation_effective=None,
        medium_velocity=0.168,
        radargram_id=None,
        radargram_name=None,
        group_id=None,
        group_name=None,
        metadata=None,
        log=None,
        source_file=None,
        antenna=None
    ))]
    fn write_netcdf(
        py: Python<'_>,
        path: Py<PyAny>,
        data: Py<PyAny>,
        twtt_ns: Py<PyAny>,
        time_zero_ns: Option<Py<PyAny>>,
        trace_time: Py<PyAny>,
        antenna_mhz: f32,
        antenna_separation: f32,
        longitude: Option<Py<PyAny>>,
        latitude: Option<Py<PyAny>>,
        x: Option<Py<PyAny>>,
        y: Option<Py<PyAny>>,
        crs: Option<String>,
        elevation: Option<Py<PyAny>>,
        dem: Option<Py<PyAny>>,
        antenna_separation_effective: Option<f32>,
        medium_velocity: f32,
        radargram_id: Option<String>,
        radargram_name: Option<String>,
        group_id: Option<String>,
        group_name: Option<String>,
        metadata: Option<Py<PyAny>>,
        log: Option<Vec<String>>,
        source_file: Option<String>,
        antenna: Option<String>,
    ) -> PyResult<String> {
        use crate::external::{Elevation, Position};
        use pyo3::exceptions::PyValueError;

        let path = fspath(py, path.bind(py))?;
        // NetCDF reports a missing directory as "Permission denied".
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            if !parent.is_dir() {
                return Err(PyValueError::new_err(format!(
                    "The directory to write {path:?} in does not exist"
                )));
            }
        }

        let numpy = py.import("numpy")?;
        let kwargs = PyDict::new(py);
        kwargs.set_item("dtype", numpy.getattr("float32")?)?;
        let data = numpy
            .getattr("asarray")?
            .call((data.bind(py),), Some(&kwargs))?
            .cast_into::<numpy::PyArray2<f32>>()
            .map_err(|_| PyValueError::new_err("data must be two-dimensional, (samples, traces)"))?
            .readonly()
            .as_array()
            .to_owned();

        let vector = |value: &Py<PyAny>, name: &str| f64_vector(py, value.bind(py), name);
        let twtt_ns = vector(&twtt_ns, "twtt_ns")?;
        let time_zero_ns = match time_zero_ns {
            None => None,
            Some(value) => {
                // A scalar is one value for every trace.
                let array = numpy.getattr("atleast_1d")?.call1((value.bind(py),))?;
                Some(f64_vector(py, &array, "time_zero_ns")?)
            }
        };
        let trace_time = epoch_seconds(py, trace_time.bind(py))?;

        let position =
            match (longitude, latitude, x, y) {
                (Some(longitude), Some(latitude), None, None) => Position::LonLat {
                    longitude: vector(&longitude, "longitude")?,
                    latitude: vector(&latitude, "latitude")?,
                    crs,
                },
                (None, None, Some(x), Some(y)) => Position::Projected {
                    x: vector(&x, "x")?,
                    y: vector(&y, "y")?,
                    crs: crs.ok_or_else(|| {
                        PyValueError::new_err("x and y need crs, the system they are in")
                    })?,
                },
                (None, None, None, None) => return Err(PyValueError::new_err(
                    "Trace positions are required: give longitude and latitude, or x, y and crs",
                )),
                _ => {
                    return Err(PyValueError::new_err(
                        "Give longitude and latitude, or x and y (with crs), and only one of \
                     those pairs",
                    ))
                }
            };
        let elevation = match (elevation, optional_path(py, dem)?) {
            (Some(values), None) => Elevation::Values(vector(&values, "elevation")?),
            (None, Some(dem)) => Elevation::Dem(dem),
            (Some(_), Some(_)) => {
                return Err(PyValueError::new_err("Give elevation or dem, not both"))
            }
            (None, None) => {
                return Err(PyValueError::new_err(
                    "Elevations are required: give elevation, or a dem to sample them from",
                ))
            }
        };

        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or_else(|| PyValueError::new_err(format!("{path:?} has no file name")))?;
        let (radargram_id, id_note) =
            crate::identity::resolve_radargram_id(radargram_id.as_deref(), None, stem)
                .map_err(PyValueError::new_err)?;
        if let Some(note) = id_note {
            py.import("warnings")?.getattr("warn")?.call1((note,))?;
        }
        let group =
            crate::identity::resolve_group(group_name.as_deref(), group_id.as_deref(), None, None)
                .map_err(PyValueError::new_err)?;
        let (group_name, group_id) = match group {
            Some((name, id)) => (Some(name), Some(id)),
            None => (None, None),
        };
        let identity = gpr::RidalIdentity {
            radargram_id: Some(radargram_id),
            display_name: crate::identity::resolve_display_name(radargram_name.as_deref(), None),
            group_name,
            group_id,
        };
        let user_metadata = optional_metadata(py, metadata)?;

        let arrays = crate::external::Arrays {
            data,
            twtt_ns,
            time_zero_ns,
            trace_time,
            position,
            elevation,
            antenna,
            antenna_mhz,
            antenna_separation,
            antenna_separation_effective,
            medium_velocity,
            log: log.unwrap_or_default(),
            source_file,
        };
        let mut gpr = crate::external::build(arrays, identity).map_err(|e| match e {
            crate::external::BuildError::Argument(e) => PyValueError::new_err(e),
            crate::external::BuildError::Dem(e) => PyRuntimeError::new_err(e),
        })?;
        gpr.user_metadata = user_metadata;
        py.detach(|| gpr.export(&path))
            .map_err(PyRuntimeError::new_err)?;
        Ok(path.to_string_lossy().into_owned())
    }

    /// Removed legacy entry point for the old Python CLI wrapper.
    ///
    /// `ridal.run_cli()` is no longer supported. Use `ridal.process()` for
    /// processing workflows and `ridal.info()` for metadata inspection.
    ///
    /// Raises
    /// ------
    /// NotImplementedError
    ///     Always raised.
    #[pyfunction(signature = (*_args, **_kwargs))]
    fn run_cli(_args: &Bound<'_, PyTuple>, _kwargs: Option<&Bound<'_, PyDict>>) -> PyResult<()> {
        Err(PyNotImplementedError::new_err(
            "ridal.run_cli() has been removed. Use ridal.process(...) for processing and ridal.info(...) for metadata inspection.",
        ))
    }
}
