//! An in-memory radargram: its data, positions and per-trace state, and
//! the operations that keep them consistent.

use std::error::Error;
use std::ops::Range;
use std::path::{Path, PathBuf};

use ndarray::{Array1, Array2, Axis};

use crate::processing::grouping::MergeKey;
use crate::{io, tools, user_metadata};

mod location;
pub mod processing_log;
#[cfg(test)]
pub(crate) mod test_fixtures;

pub use location::{CorPoint, GPRLocation, LocationCorrection, TimeSource};

/// Which geometry `correct_antenna_separation` converts travel time to
/// depth with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SeparationMethod {
    /// Two slant legs below the antenna midpoint, timed from the air wave's
    /// arrival at the receiver. See [`tools::return_time_to_depth`].
    #[default]
    Slant,
    /// The pre-0.7 conversion, kept so that data processed with it -- and
    /// picks made on its grid -- can be regenerated exactly. It uses the
    /// full separation where the geometry needs half, and shrinks the
    /// separation by time zero times the medium velocity, which after a
    /// zero correction removes it altogether (#261).
    Legacy,
}

impl std::fmt::Display for SeparationMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            SeparationMethod::Slant => "slant",
            SeparationMethod::Legacy => "legacy",
        })
    }
}

impl std::str::FromStr for SeparationMethod {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "slant" => Ok(SeparationMethod::Slant),
            "legacy" => Ok(SeparationMethod::Legacy),
            _ => Err("expected `slant` or `legacy`".into()),
        }
    }
}

/// Metadata associated with a GPR dataset
///
/// This contains all required information except the location data and the actual data
#[derive(Debug, Clone)]
pub struct GPRMeta {
    /// The number of samples per trace (the vertical data size)
    pub samples: u32,
    /// The control unit sampling frequency (MHz)
    pub frequency: f32,
    pub frequency_steps: u32,
    /// The interval between traces (s)
    pub time_interval: f32,
    /// The name of the antenna
    pub antenna: String,
    /// The frequency of the antenna (MHz)
    pub antenna_mhz: f32,
    /// The horizontal separation between the antenna transmitter and receiver (m)
    pub antenna_separation: f32,
    /// The return time window (ns)
    pub time_window: f32,
    /// The number of traces in the data (the horizontal data size)
    pub last_trace: u32,
    /// The path to the data file
    pub data_filepath: PathBuf,
    /// The velocity of the medium (m / ns)
    pub medium_velocity: f32,
}

impl GPRMeta {
    /// Find a ".cor" file based on the location of the ".rd3" file
    ///
    /// # Arguments
    /// - `projected_crs`: The CRS to project coordinates into
    #[cfg(test)]
    pub fn find_cor(&self, projected_crs: Option<&String>) -> Result<GPRLocation, Box<dyn Error>> {
        let cor = crate::formats::find_neighbor_case_insensitive(&self.data_filepath, "cor")
            .unwrap_or_else(|| self.data_filepath.with_extension("cor"));
        io::load_cor(&cor, projected_crs)
    }
}

impl std::fmt::Display for GPRMeta {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "
GPR Metadata
------------
Filepath:\t\t{:?}
Samples (height):\t{}
Traces (width):\t\t{}
Time window:\t\t{} ns
Max depth:\t\t{:.1} m
Medium velocity:\t{} m/ns
Sampling frequency:\t{} MHz
Time between traces:\t{} s
Antenna:\t\t{}
Antenna separation:\t{} m
",
            self.data_filepath,
            self.samples,
            self.last_trace,
            self.time_window,
            0.5 * self.time_window * self.medium_velocity,
            self.medium_velocity,
            self.frequency,
            self.time_interval,
            self.antenna,
            self.antenna_separation,
        )
    }
}

/// An in-memory GPR dataset
#[allow(clippy::upper_case_acronyms)]
pub struct GPR {
    /// The data matrix, with rows of traces (originally in mV)
    pub data: Array2<f32>,
    /// Topographically corrected data matrix, if processed.
    pub topo_data: Option<Array2<f32>>,
    /// Trace X/Y/Z location data
    pub location: GPRLocation,
    /// Metadata for the GPR dataset
    pub metadata: GPRMeta,
    /// Processing log. Each line is one processing step
    pub log: Vec<String>,
    /// The steps that the user provided
    pub steps: Vec<String>,
    /// The antenna separation that the data still needs correcting for (m).
    ///
    /// Starts as [`GPRMeta::antenna_separation`], the acquisition fact, and
    /// is zeroed by [`crate::processing::steps::geometry::run_correct_antenna_separation`] once the geometry
    /// has been taken out of the data. The two are therefore different
    /// quantities: one is what the instrument did, this is processing
    /// state, and only this one says what `twtt` currently refers to.
    antenna_separation_effective: f32,
    /// Where each trace's first sample sits on the original recording's
    /// clock, in nanoseconds, one value per trace.
    ///
    /// A position, not an amount removed. The two are numerically the same
    /// while a zero correction crops exactly at time zero, and part company
    /// when it keeps a margin above it or something crops further (#152).
    ///
    /// Per trace rather than one number because `zero_corr(<method>, trace)`
    /// aligns each trace's first break on its own, and a single
    /// value would be the mean of crops that no individual trace actually
    /// received. That mean is not a rounding error: it is wrong by the
    /// spread of the first breaks, on the axis whose origin re-anchoring
    /// depends on.
    ///
    /// Invariant: `crop_ns.len() == width()`. Every operation that changes
    /// the trace count maintains it alongside `location.cor_points`, which
    /// has the same shape and the same rule.
    crop_ns: Vec<f32>,
    /// Where time zero -- the direct wave's arrival at the receiver -- sits on
    /// the recording clock, in nanoseconds, per trace.
    ///
    /// Set only by a zero correction, which is the only thing that locates
    /// it. `0` means it has never been located, which is the honest reading
    /// of an uncorrected file: its travel times are measured from whenever
    /// the instrument started sampling.
    ///
    /// Distinct from `crop_ns` because "how much was thrown away" and
    /// "where zero is" are different facts. They are equal after a zero
    /// correction that keeps no margin above time zero, and part company
    /// with a margin (`zero_corr(margin=...)`) or a later crop (`subset`)
    /// (#152).
    ///
    /// Same length invariant, maintained by the same five operations.
    time_zero_ns: Vec<f32>,
    /// User-supplied metadata that should be carried through processing/export.
    pub user_metadata: user_metadata::UserMetadata,
    /// Persistent radargram identity metadata (#116). Resolved once, at the
    /// end of [`build_processed_gpr`], from CLI arguments and the output
    /// stem -- not populated on GPR instances still mid-processing.
    pub identity: RidalIdentity,
    /// The raw files and settings this radargram was read with, when it
    /// was read from raw files (#382). With [`GPR::steps`], they make the
    /// recipe that export records and the revision id is derived from.
    pub inputs: Option<crate::recipe::Inputs>,
}

/// New traces and everything that describes them per trace, for
/// [`GPR::replace_traces`].
///
/// Named fields rather than arguments, because `crop_ns` and `time_zero_ns`
/// have the same type and a swap would go unnoticed.
pub(crate) struct Traces {
    pub data: Array2<f32>,
    pub topo_data: Option<Array2<f32>>,
    pub cor_points: Vec<CorPoint>,
    pub crop_ns: Vec<f32>,
    pub time_zero_ns: Vec<f32>,
}

/// Persistent identity metadata for one processed output. See
/// [`crate::identity`] for the concepts and their precedence rules.
#[derive(Debug, Clone, Default)]
pub struct RidalIdentity {
    pub radargram_id: Option<crate::identity::RadargramId>,
    pub display_name: Option<crate::identity::DisplayName>,
    pub group_name: Option<crate::identity::GroupName>,
    pub group_id: Option<crate::identity::GroupId>,
}

impl GPR {
    /// The antenna separation the data has **not** yet been corrected for,
    /// in metres.
    ///
    /// Equal to `metadata.antenna_separation` until
    /// [`crate::processing::steps::geometry::run_correct_antenna_separation`] runs, and zero afterwards. This
    /// is the one that says what `twtt` currently refers to; the metadata
    /// field is acquisition provenance and never changes.
    pub fn antenna_separation_effective(&self) -> f32 {
        self.antenna_separation_effective
    }

    /// Where each trace's first sample sits on the recording clock, in
    /// nanoseconds: how much of the front of the record was discarded.
    ///
    /// One value per trace, because `zero_corr(<method>, trace)` crops per trace.
    /// See [`GPR::twtt_crop_uniform_ns`] for the common case where they all
    /// agree, and [`GPR::twtt_time_zero_ns`] for where time zero sits.
    pub fn twtt_crop_ns(&self) -> &[f32] {
        &self.crop_ns
    }

    /// The one crop that describes every trace, when there is one.
    ///
    /// `None` when they differ, which is the per-trace `zero_corr` case. The
    /// export writes an `(x)`-dimensioned variable then, rather than a mean
    /// that is true of no trace.
    pub fn twtt_crop_uniform_ns(&self) -> Option<f32> {
        let first = *self.crop_ns.first()?;
        self.crop_ns
            .iter()
            .all(|v| (v - first).abs() < f32::EPSILON)
            .then_some(first)
    }

    /// Where time zero -- the direct wave's arrival at the receiver -- sits on
    /// the recording clock, per trace.
    ///
    /// `0` until a zero correction locates it, which is the honest reading
    /// of an uncorrected file: its travel times are measured from whenever
    /// the instrument started sampling.
    ///
    /// On the **same clock as the crop**, not an offset from sample 0. A
    /// zero correction without a margin therefore sets this *equal to*
    /// `crop_ns` rather than to zero — sample 0 becomes the pulse, so the
    /// travel time of sample 0 is zero; with a margin it is the margin
    /// later. Expecting `0` here after a correction is the
    /// natural reading and the wrong one, and it matters: under that
    /// reading a corrected and an uncorrected radargram would be
    /// indistinguishable, and carrying picks between them would report no
    /// movement while the data had shifted by the whole correction. That
    /// bug was reported against real data before this paragraph existed.
    ///
    /// The zero sentinel is what
    /// [`crate::interp::anchors::twtt_axis`] reads to decide that a
    /// radargram has no anchored travel-time axis at all.
    ///
    /// Named `time_zero` rather than `t0` on purpose. gprinterp's `t0` for
    /// the `twtt` axis is **neither this nor the crop** -- it is their
    /// difference, `crop - time_zero`, the travel-time value of sample 0 --
    /// and a reviewer with the SPEC in front of them substituted one for
    /// the other anyway. A name nobody reaches for by mistake is cheaper
    /// than the comment explaining why they should not have.
    pub fn twtt_time_zero_ns(&self) -> &[f32] {
        &self.time_zero_ns
    }

    /// The one time zero that describes every trace, when there is one.
    pub fn twtt_time_zero_uniform_ns(&self) -> Option<f32> {
        let first = *self.time_zero_ns.first()?;
        self.time_zero_ns
            .iter()
            .all(|v| (v - first).abs() < f32::EPSILON)
            .then_some(first)
    }

    /// Which gprinterp `y` anchor axis (SPEC §8.2) the `twtt` coordinate is.
    ///
    /// After an antenna-separation correction, `twtt` is no longer travel
    /// time between the antenna pair but travel time to and from a
    /// theoretical coincident antenna. Both are linear in sample index and
    /// both hold plausible nanosecond values, so a consumer cannot tell
    /// them apart from the numbers -- and re-anchoring one onto the other
    /// is wrong by the antenna geometry while looking entirely reasonable.
    ///
    /// Inferred here rather than tracked as a flag because Ridal owns both
    /// numbers and the inference is exact: the correction is the only thing
    /// that zeroes the effective separation, and it refuses to run when the
    /// acquisition separation is already zero. A radargram acquired at zero
    /// separation is reported as `twtt`, which is both true -- nothing was
    /// corrected -- and the conservative direction, since a corrected file
    /// is never mistaken for an uncorrected one.
    pub fn twtt_anchor_name(&self) -> &'static str {
        let corrected =
            self.antenna_separation_effective == 0. && self.metadata.antenna_separation > 0.;
        if corrected {
            "twtt_normal_incidence"
        } else {
            "twtt"
        }
    }

    pub fn vertical_resolution_ns(&self) -> f32 {
        self.metadata.time_window / self.metadata.samples as f32
    }

    pub fn from_meta_and_loc(
        location: GPRLocation,
        metadata: GPRMeta,
    ) -> Result<GPR, Box<dyn Error>> {
        let data = match metadata.data_filepath.extension().and_then(|s| s.to_str()) {
            Some(ext) if ext.eq_ignore_ascii_case("rd3") => Ok(io::load_rd3(
                &metadata.data_filepath,
                metadata.samples as usize,
            )?),
            Some(ext) if ext.eq_ignore_ascii_case("dt1") => Ok(io::load_pe_dt1(
                &metadata.data_filepath,
                metadata.samples as usize,
                metadata.last_trace as usize,
            )?),
            Some(ext) if ext.eq_ignore_ascii_case("dzt") => Ok(io::load_dzt(
                &metadata.data_filepath,
                metadata.samples as usize,
            )?),
            _ => Err(format!("Unknown filetype: {:?}", metadata.data_filepath)),
        }?;

        let n_traces = data.shape()[1];
        if n_traces == 0 {
            return Err("the data file holds no traces".into());
        }
        let location_data = match data.shape()[1] == location.cor_points.len() {
            true => location,
            false => location.range_fill(0, data.shape()[1] as u32, metadata.time_interval),
        };
        let antenna_separation_effective = metadata.antenna_separation;

        Ok(GPR {
            data,
            location: location_data,
            metadata,
            log: Vec::new(),
            steps: Vec::new(),
            topo_data: None,
            antenna_separation_effective,
            crop_ns: vec![0.; n_traces],
            time_zero_ns: vec![0.; n_traces],
            user_metadata: user_metadata::UserMetadata::new(),
            identity: RidalIdentity::default(),
            inputs: None,
        })
    }

    /// A radargram whose data and vertical axis were made elsewhere: what
    /// [`crate::external::build`] assembles from arrays.
    ///
    /// The caller has already checked every argument against the others,
    /// including the length invariants of `crop_ns` and `time_zero_ns`;
    /// this only puts them where the rest of `GPR` expects them.
    pub(crate) fn from_external(
        data: Array2<f32>,
        location: GPRLocation,
        metadata: GPRMeta,
        antenna_separation_effective: f32,
        crop_ns: Vec<f32>,
        time_zero_ns: Vec<f32>,
        log: Vec<String>,
    ) -> GPR {
        GPR {
            data,
            location,
            metadata,
            log,
            steps: Vec::new(),
            topo_data: None,
            antenna_separation_effective,
            crop_ns,
            time_zero_ns,
            user_metadata: user_metadata::UserMetadata::new(),
            identity: RidalIdentity::default(),
            inputs: None,
        }
    }

    /// Render this radargram to an image with a render profile.
    ///
    /// Goes through the same pipeline as `ridal render` and the web GUI --
    /// same profiles, same resampling, same amplitude sampling -- from the
    /// array already in memory, since `--no-export --render` is allowed and
    /// then there is no file to read back.
    ///
    /// # Always `data`, never `data_topocorr`
    ///
    /// The renderer this replaced drew the topographically corrected array
    /// whenever topographic correction had run. That made one output file
    /// have two pictures: this one, and the different one the browser drew
    /// from the exported `data` variable.
    ///
    /// One array wins instead, and it is `data`, so every way of drawing a
    /// radargram agrees. The corrected view is an explicit request, `topo`
    /// (`--render-topo`, `render_topo=True`), not a thing `--render`
    /// silently switches to because an earlier step happened to run -- and
    /// it too is drawn from `data`, sheared by the elevation and depth axes
    /// this radargram exports, exactly as `ridal render --topo` and the
    /// browser's corrected view draw it (#289).
    ///
    /// This replaced a separate implementation (`io::render_jpg`) that
    /// stretched between the 1st and 99th percentile of every tenth sample
    /// and clamped the floor to zero when `unphase` had run. Two renderers
    /// that disagree about how to draw the same data is a bad property for
    /// an instrument display, and the `unphase` case was a fixed rule
    /// applied regardless of what the amplitudes actually looked like.
    pub fn render(
        &self,
        filepath: &Path,
        profile: &crate::render::profile::RenderProfile,
        width: Option<usize>,
        topo: bool,
    ) -> Result<(), Box<dyn Error>> {
        let source = crate::source::ArraySource::new(self.data.view());
        let request = crate::render::oneshot::RenderRequest {
            profile,
            width,
            quality: None,
        };
        if topo {
            let elevation: Vec<f64> = self
                .location
                .cor_points
                .iter()
                .map(|p| p.altitude)
                .collect();
            let depth = self.depths().to_vec();
            crate::render::oneshot::render_topo_to_file(
                &source,
                Some(&elevation),
                Some(&depth),
                filepath,
                &request,
            )?;
        } else {
            crate::render::oneshot::render_to_file(&source, filepath, &request)?;
        }
        Ok(())
    }

    /// Put `data` in place and make the sample count, time window and trace
    /// count say what it is. The trace-count invariants are the caller's;
    /// only the primitives below call this.
    fn update_data(&mut self, data: Array2<f32>) {
        self.data = data;

        self.metadata.time_window *= self.height() as f32 / self.metadata.samples as f32;
        self.metadata.samples = self.height() as u32;
        self.metadata.last_trace = self.width() as u32;
    }

    // The primitives below are the only writers of the per-trace state
    // (`crop_ns`, `time_zero_ns`, `cor_points`, `topo_data`'s columns),
    // apart from construction and `merge`. Each keeps one entry per trace,
    // so a step built on them cannot leave the metadata describing traces
    // that are no longer there (#85).

    /// A copy of the traces `traces` and samples `samples`.
    ///
    /// Cropping leading samples moves the origin of the travel-time axis
    /// just as a zero correction does, so it adds to the crop. Time zero
    /// does not move: the direct wave arrived when it arrived, whatever is
    /// later done to the record. This is where the two quantities part
    /// company, and the gap is exactly the travel time of the new sample 0.
    ///
    /// A copy rather than a change in place, because `GPR` is deliberately
    /// not `Clone` and cutting several windows from one recording is what
    /// the step digests do.
    ///
    /// # Panics
    ///
    /// If either range reaches past the data; `subset` checks first.
    pub(crate) fn window(&self, traces: Range<usize>, samples: Range<usize>) -> GPR {
        // Measured against the window and height before the cut, which are
        // what `samples` indexes into.
        let removed_ns =
            samples.start as f32 * self.metadata.time_window / self.height().max(1) as f32;
        let mut metadata = self.metadata.clone();
        metadata.time_window *= samples.len() as f32 / metadata.samples as f32;
        metadata.samples = samples.len() as u32;
        metadata.last_trace = traces.len() as u32;
        GPR {
            data: self
                .data
                .slice(ndarray::s![samples, traces.clone()])
                .to_owned(),
            topo_data: self
                .topo_data
                .as_ref()
                .map(|topo| topo.slice(ndarray::s![.., traces.clone()]).to_owned()),
            location: GPRLocation {
                cor_points: self.location.cor_points[traces.clone()].to_vec(),
                correction: self.location.correction.clone(),
                time_source: self.location.time_source,
                crs: self.location.crs.clone(),
            },
            metadata,
            log: self.log.clone(),
            steps: self.steps.clone(),
            antenna_separation_effective: self.antenna_separation_effective,
            crop_ns: self.crop_ns[traces.clone()]
                .iter()
                .map(|crop| crop + removed_ns)
                .collect(),
            time_zero_ns: self.time_zero_ns[traces].to_vec(),
            user_metadata: self.user_metadata.clone(),
            identity: self.identity.clone(),
            inputs: self.inputs.clone(),
        }
    }

    /// Keep the traces `indices`, in that order, with everything that
    /// describes them.
    ///
    /// # Panics
    ///
    /// If an index is not a trace; callers check their indices first.
    pub(crate) fn select_traces(&mut self, indices: &[usize]) {
        self.data = self.data.select(Axis(1), indices);
        if let Some(topo_data) = &self.topo_data {
            self.topo_data = Some(topo_data.select(Axis(1), indices));
        }
        self.location.cor_points = indices
            .iter()
            .map(|&i| self.location.cor_points[i])
            .collect();
        self.crop_ns = indices.iter().map(|&i| self.crop_ns[i]).collect();
        self.time_zero_ns = indices.iter().map(|&i| self.time_zero_ns[i]).collect();
        self.metadata.last_trace = self.width() as u32;
    }

    /// Replace the traces, and everything that describes them, with new
    /// ones the step computed.
    ///
    /// Refused unless every per-trace quantity has one entry per trace of
    /// `traces.data`.
    pub(crate) fn replace_traces(&mut self, traces: Traces) -> Result<(), String> {
        let width = traces.data.ncols();
        let topo_width = traces.topo_data.as_ref().map(|topo| topo.ncols());
        for (what, len) in [
            ("coordinates", Some(traces.cor_points.len())),
            ("crop offsets", Some(traces.crop_ns.len())),
            ("time-zero offsets", Some(traces.time_zero_ns.len())),
            ("topographically corrected traces", topo_width),
        ] {
            if let Some(len) = len.filter(|len| *len != width) {
                return Err(format!(
                    "Internal error: {len} {what} for {width} new traces"
                ));
            }
        }
        self.topo_data = traces.topo_data;
        self.location.cor_points = traces.cor_points;
        self.crop_ns = traces.crop_ns;
        self.time_zero_ns = traces.time_zero_ns;
        self.update_data(traces.data);
        Ok(())
    }

    /// Replace the samples of every trace, keeping the traces.
    ///
    /// The number of samples may change, and the time window follows it.
    /// Refused if the number of traces changes.
    pub(crate) fn replace_samples(&mut self, data: Array2<f32>) -> Result<(), String> {
        if data.ncols() != self.width() {
            return Err(format!(
                "Internal error: {} traces replaced by {}",
                self.width(),
                data.ncols()
            ));
        }
        self.update_data(data);
        Ok(())
    }

    /// Replace the samples with `data`, which is these traces with the
    /// first `removed_rows[i]` samples of trace `i` taken off (and the
    /// bottom trimmed as the step chose), and add what was taken off to the
    /// crop. Time zero does not move; see [`GPR::window`].
    pub(crate) fn crop_samples(
        &mut self,
        data: Array2<f32>,
        removed_rows: &[usize],
    ) -> Result<(), String> {
        if removed_rows.len() != self.width() {
            return Err(format!(
                "Internal error: {} crops for {} traces",
                removed_rows.len(),
                self.width()
            ));
        }
        // Measured against the window and height before the crop, which
        // are what the rows index into.
        let ns_per_row = self.metadata.time_window / self.height().max(1) as f32;
        let crop_ns: Vec<f32> = self
            .crop_ns
            .iter()
            .zip(removed_rows)
            .map(|(crop, rows)| crop + *rows as f32 * ns_per_row)
            .collect();
        self.replace_samples(data)?;
        self.crop_ns = crop_ns;
        Ok(())
    }

    /// Say where time zero is on the recording clock, in nanoseconds, per
    /// trace. Only a zero correction locates it.
    pub(crate) fn set_time_zero_ns(&mut self, time_zero_ns: Vec<f32>) -> Result<(), String> {
        if time_zero_ns.len() != self.width() {
            return Err(format!(
                "Internal error: {} time-zero offsets for {} traces",
                time_zero_ns.len(),
                self.width()
            ));
        }
        self.time_zero_ns = time_zero_ns;
        Ok(())
    }

    /// Record that the antenna separation has been taken out of the data,
    /// so that `twtt` now refers to a coincident antenna.
    pub(crate) fn mark_separation_corrected(&mut self) {
        self.antenna_separation_effective = 0.;
    }

    pub fn height(&self) -> usize {
        self.data.shape()[0]
    }
    pub fn width(&self) -> usize {
        self.data.shape()[1]
    }

    pub fn export(&self, nc_filepath: &Path) -> Result<(), String> {
        let ds = self.export_dataset()?;
        io::export_netcdf(&ds, nc_filepath)
    }

    /// Travel time of the first sample, in nanoseconds.
    ///
    /// `crop - time_zero`: zero right after a zero correction that crops
    /// exactly at time zero, negative when it kept a margin above time
    /// zero, and positive once something crops further. One number for the
    /// whole radargram, because every trace is aligned on its own time zero
    /// and so has the same difference; the mean guards the export against
    /// a pipeline that breaks that.
    pub fn twtt_first_sample_ns(&self) -> f32 {
        if self.crop_ns.is_empty() {
            return 0.;
        }
        self.crop_ns
            .iter()
            .zip(&self.time_zero_ns)
            .map(|(crop, time_zero)| crop - time_zero)
            .sum::<f32>()
            / self.crop_ns.len() as f32
    }

    /// Travel time of every sample, in nanoseconds.
    pub fn twtt_ns(&self) -> Array1<f32> {
        let first = self.twtt_first_sample_ns();
        let step = self.vertical_resolution_ns();
        Array1::from_iter((0..self.height()).map(|i| first + i as f32 * step))
    }

    /// Depth of every sample, in metres. Samples above time zero (a margin
    /// kept by `zero_corr`) get negative depths, as a straight path through
    /// the medium, so the axis keeps increasing.
    ///
    /// The geometry is [`tools::return_time_to_depth`]'s, with the separation
    /// the data has not yet been corrected for. Where time zero sits on the
    /// recording clock plays no part: that is mostly the instrument's
    /// pre-trigger delay, not a flight time (#261). Before a zero correction,
    /// sample 0 stands in for time zero.
    ///
    /// Time zero is taken to be on the air wave. Only
    /// `correct_antenna_separation` can be told otherwise, and after it has
    /// run there is no separation left for the choice to matter.
    pub fn depths(&self) -> Array1<f32> {
        self.depths_timed_from(tools::SPEED_OF_LIGHT_AIR_M_PER_NS)
    }

    /// [`GPR::depths`], with time zero on a direct wave travelling at
    /// `direct_velocity` (m/ns).
    pub(crate) fn depths_timed_from(&self, direct_velocity: f32) -> Array1<f32> {
        self.twtt_ns().mapv(|time| {
            if time < 0. {
                time * self.metadata.medium_velocity / 2.
            } else {
                tools::return_time_to_depth(
                    time,
                    self.metadata.medium_velocity,
                    self.antenna_separation_effective,
                    direct_velocity,
                )
            }
        })
    }

    /// [`GPR::depths`] as it was before #261, for
    /// [`SeparationMethod::Legacy`] only.
    pub(crate) fn legacy_depths(&self) -> Array1<f32> {
        let velocity = self.metadata.medium_velocity;
        let time_zero_mean = match self.time_zero_ns.len() {
            0 => 0.,
            n => self.time_zero_ns.iter().sum::<f32>() / n as f32,
        };
        let separation = (self.antenna_separation_effective.powi(2)
            - (time_zero_mean * velocity).powi(2))
        .max(0.)
        .sqrt();
        self.twtt_ns().mapv(|time| {
            let two_way_distance = time * velocity;
            if time < 0. {
                two_way_distance / 2.
            } else if two_way_distance > 2. * separation {
                (two_way_distance.powi(2) - 4. * separation.powi(2)).sqrt() / 2.
            } else {
                0.
            }
        })
    }

    /// Append `other`'s traces to these.
    ///
    /// Refused, naming both values, when anything the merged file would
    /// state once for all its traces differs: the CRS, antenna frequency,
    /// time window, antenna separation (nominal and effective) or medium
    /// velocity.
    pub fn merge(&mut self, other: &GPR) -> Result<(), String> {
        let start_time = std::time::Instant::now();
        if let Some(refusal) = MergeKey::of(&self.metadata, &self.location)
            .refusal(&MergeKey::of(&other.metadata, &other.location))
        {
            Err(refusal)
        } else if self.antenna_separation_effective != other.antenna_separation_effective {
            // Unlike the others this is recoverable, so say how.
            Err(format!(
                "Effective antenna separations are different: {} m vs {} m. One radargram \
                 has had correct_antenna_separation applied and the other has not; apply it \
                 to both before merging",
                self.antenna_separation_effective, other.antenna_separation_effective
            ))
        } else {
            self.location
                .cor_points
                .append(other.location.cor_points.clone().as_mut());
            self.location.time_source = self.location.time_source.and(other.location.time_source);

            self.data.append(Axis(1), other.data.view()).unwrap();
            // Per trace, so it concatenates with the traces. Two inputs
            // cropped by different amounts is exactly the case a single
            // number could not describe.
            self.crop_ns.extend_from_slice(&other.crop_ns);
            self.time_zero_ns.extend_from_slice(&other.time_zero_ns);

            self.metadata.time_window *= self.height() as f32 / self.metadata.samples as f32;
            self.metadata.samples = self.height() as u32;
            self.metadata.last_trace = self.width() as u32;
            user_metadata::merge_prefer_first(&mut self.user_metadata, &other.user_metadata);

            // Not a step, but recorded like one: `export` reads the merged
            // files back from these lines.
            self.log.push(processing_log::line(
                "merge",
                start_time.elapsed(),
                &format!("Merged {:?}", other.metadata.data_filepath),
            ));

            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::gpr::test_fixtures::*;

    #[test]
    fn merge_refuses_what_the_merged_file_could_not_state_once() {
        let base = || make_dummy_gpr(20, 10, Some(1.));

        let mut other = base();
        other.metadata.antenna_separation = 4.;
        let message = base().merge(&other).unwrap_err();
        assert!(
            message.contains("Antenna separations are different"),
            "{message}"
        );

        // Corrected on one side only: recoverable, and the message says how.
        let mut other = base();
        other.antenna_separation_effective = 0.;
        let message = base().merge(&other).unwrap_err();
        assert!(message.contains("correct_antenna_separation"), "{message}");

        let mut other = base();
        other.metadata.medium_velocity = 0.1;
        let message = base().merge(&other).unwrap_err();
        assert!(
            message.contains("Medium velocities are different"),
            "{message}"
        );

        // Differently cropped inputs are fine: the offsets are per trace.
        let mut other = base();
        other.crop_ns = vec![3.; other.width()];
        let mut merged = base();
        merged.merge(&other).unwrap();
        assert_eq!(merged.crop_ns.len(), 40);
    }

    #[test]
    fn render_topo_draws_the_corrected_view_from_memory() {
        // #289: `process --render-topo` shears the array in memory by the
        // elevation and depth it exports. The test track climbs a metre per
        // trace, so the corrected raster is taller than the standard one.
        let gpr = make_test_gpr(Some(50), Some(40));
        let dir = tempfile::tempdir().unwrap();
        let profile = crate::render::profile::RenderProfile::default_profile();
        let standard = dir.path().join("standard.png");
        let topo = dir.path().join("topo.png");

        gpr.render(&standard, &profile, None, false).unwrap();
        gpr.render(&topo, &profile, None, true).unwrap();

        let (standard_w, standard_h) = image::image_dimensions(&standard).unwrap();
        let (topo_w, topo_h) = image::image_dimensions(&topo).unwrap();
        assert_eq!((standard_w, standard_h), (50, 40));
        assert_eq!(topo_w, standard_w);
        assert!(
            topo_h > standard_h,
            "{topo_h} is not taller than {standard_h}"
        );
    }

    #[test]
    fn test_make_test_gpr() {
        let gpr = make_test_gpr(None, None);

        assert_eq!(gpr.data[[0, 0]], 0.);
        assert_eq!(
            gpr.data[[gpr.data.shape()[0] - 1, 0]],
            (gpr.data.shape()[0] - 1) as f32
        );
    }

    /// Every attribute Ridal generates on its own must be ASCII (#256).
    ///
    /// A non-ASCII `NC_CHAR` attribute is flagged `H5T_CSET_ASCII` by HDF5,
    /// so readers that trust the flag -- xarray with the `h5netcdf` engine,
    /// `h5dump` -- garble the UTF-8 bytes. Ridal's own output must therefore
    /// never be the source of the problem. A user-supplied display or group
    /// name is the one deliberate exception, covered by the NetCDF
    /// round-trip test in `io.rs`.
    #[test]
    fn ridal_generated_attributes_are_ascii_by_default() {
        let gpr = make_exportable_gpr(16, 32);
        let ds = gpr.export_dataset().unwrap();
        for (name, value) in &ds.attrs {
            let text = match value {
                crate::export::ExportAttr::String(s) => s.clone(),
                crate::export::ExportAttr::Strings(v) => v.join("\n"),
                _ => continue,
            };
            assert!(
                text.is_ascii(),
                "generated attribute '{name}' is not ASCII: {text:?}"
            );
        }
    }

    #[test]
    fn the_export_distinguishes_acquired_separation_from_remaining_separation() {
        // Before the correction the two agree, and `twtt` is what the
        // antenna pair recorded.
        let mut gpr = make_exportable_gpr(64, 256);
        let acquired = gpr.metadata.antenna_separation;
        assert!(acquired > 0., "the fixture needs a real separation");

        let before = gpr.export_dataset().unwrap();
        assert_eq!(f32_attr(&before, "antenna_separation"), acquired);
        assert_eq!(f32_attr(&before, "antenna_separation_effective"), acquired);
        assert_eq!(
            str_attr(&before.coords["twtt"].attrs, "anchor_name"),
            "twtt"
        );

        crate::processing::steps::geometry::run_correct_antenna_separation(
            &mut gpr,
            super::SeparationMethod::Slant,
            crate::tools::SPEED_OF_LIGHT_AIR_M_PER_NS,
        )
        .unwrap();
        let after = gpr.export_dataset().unwrap();

        // The acquisition fact survives -- it is still true that the survey
        // was flown with the antennas that far apart.
        assert_eq!(
            f32_attr(&after, "antenna_separation"),
            acquired,
            "acquisition provenance must not be rewritten by processing"
        );
        // What changed is how much of it the data still carries. Exporting
        // the acquisition value here is what would make a reader
        // double-correct.
        assert_eq!(f32_attr(&after, "antenna_separation_effective"), 0.);
        assert_eq!(
            str_attr(&after.coords["twtt"].attrs, "anchor_name"),
            "twtt_normal_incidence",
            "twtt now refers to a coincident antenna, not the pair"
        );
    }

    #[test]
    fn successive_crops_accumulate_into_one_crop() {
        let mut accumulating = make_gpr_with_first_break(64, 512);
        crate::processing::steps::zero_corr::run_zero_corr_legacy(&mut accumulating, 1.).unwrap();
        let first = accumulating.twtt_crop_uniform_ns().unwrap();
        assert!(first > 0., "the fixture must have a first break to find");

        // The same second crop, measured on a radargram that arrives at
        // the same data carrying no history.
        let mut in_isolation = make_gpr_with_first_break(64, 512);
        crate::processing::steps::zero_corr::run_zero_corr_legacy(&mut in_isolation, 1.).unwrap();
        in_isolation.crop_ns.fill(0.);
        in_isolation.time_zero_ns.fill(0.);
        crate::processing::steps::zero_corr::run_zero_corr_legacy(&mut in_isolation, 1.).unwrap();
        let second = in_isolation.twtt_crop_uniform_ns().unwrap();
        assert!(second > 0., "the second crop must remove something too");

        crate::processing::steps::zero_corr::run_zero_corr_legacy(&mut accumulating, 1.).unwrap();

        // Assigning instead of accumulating would report `second` alone and
        // describe the file as starting at the later offset — the same
        // silently shifted origin SPEC §7.5.1 exists to prevent, arrived at
        // from inside one pipeline rather than between two.
        let total = accumulating.twtt_crop_uniform_ns().unwrap();
        assert!(
            (total - (first + second)).abs() < 1e-4,
            "expected {first} + {second}, got {total}"
        );
    }

    #[test]
    fn a_margin_above_time_zero_has_negative_travel_times_and_depths() {
        let mut gpr = make_gpr_with_first_break(16, 512);
        let step = gpr.vertical_resolution_ns();
        gpr.process(&format!("zero_corr(max_peak, margin={})", 4. * step))
            .unwrap();

        let crop = gpr.twtt_crop_uniform_ns().unwrap();
        let time_zero = gpr.twtt_time_zero_uniform_ns().unwrap();
        assert!((time_zero - crop - 4. * step).abs() < 1e-3);
        // Row 4 is time zero, and the rows above it are before it.
        let twtt = gpr.twtt_ns();
        assert!((twtt[0] + 4. * step).abs() < 1e-3, "{}", twtt[0]);
        assert!(twtt[4].abs() < 1e-3, "{}", twtt[4]);
        let depths = gpr.depths();
        assert!(depths[0] < 0. && depths[3] < 0., "{depths:?}");
        assert!(depths.windows(2).into_iter().all(|w| w[1] >= w[0]));

        // And the file says the same.
        let exported = gpr.export_dataset().unwrap();
        match &exported.coords["twtt"].data {
            crate::export::ExportArray::F32Owned1D(values) => {
                assert!((values[0] + 4. * step).abs() < 1e-3, "{}", values[0])
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn subsetting_away_leading_samples_adds_to_the_crop() {
        // `subset(0 -1 <n> -1)` crops the front of every trace exactly as a
        // zero correction does, so it has to report the same way. Reporting
        // zero here would describe the file as starting at the original
        // time zero when it starts `n` samples later -- the silently
        // shifted origin SPEC §7.5.1 exists to prevent, reached by a
        // pipeline that never ran a zero correction at all.
        let gpr = make_gpr_with_first_break(16, 512);
        let step = gpr.metadata.time_window / gpr.height() as f32;
        assert_eq!(gpr.twtt_crop_uniform_ns().unwrap(), 0.);

        let cropped = subset_of(&gpr, None, None, Some(40), None).unwrap();
        let expected = 40.0 * step;
        assert!(
            (cropped.twtt_crop_uniform_ns().unwrap() - expected).abs() < 1e-4,
            "expected {expected}, got {}",
            cropped.twtt_crop_uniform_ns().unwrap()
        );

        // And it accumulates with everything else, so two crops in one
        // pipeline still describe one origin.
        let twice = subset_of(&cropped, None, None, Some(10), None).unwrap();
        let second_step = cropped.metadata.time_window / cropped.height() as f32;
        let total = expected + 10.0 * second_step;
        assert!(
            (twice.twtt_crop_uniform_ns().unwrap() - total).abs() < 1e-4,
            "expected {total}, got {}",
            twice.twtt_crop_uniform_ns().unwrap()
        );

        // Trimming the tail changes the window, not the origin.
        let tail = subset_of(&gpr, None, None, None, Some(100)).unwrap();
        assert_eq!(tail.twtt_crop_uniform_ns().unwrap(), 0.);
    }

    #[test]
    fn cropping_further_moves_the_crop_and_leaves_time_zero_where_it_is() {
        // The reason there are two numbers. A zero correction locates time
        // zero and crops to it, so the two agree. Anything that crops
        // afterwards moves the record without moving the moment the pulse
        // left the antenna, and their difference becomes the travel time of
        // the new first sample -- which is precisely the gprinterp anchor
        // `t0` that neither of them is.
        let mut gpr = make_gpr_with_first_break(16, 512);
        crate::processing::steps::zero_corr::run_zero_corr_legacy(&mut gpr, 1.).unwrap();
        let at_zero = gpr.twtt_crop_uniform_ns().unwrap();
        assert!(at_zero > 0.);
        assert_eq!(
            gpr.twtt_time_zero_uniform_ns().unwrap(),
            at_zero,
            "the crop landed on time zero, so they agree"
        );

        let step = gpr.metadata.time_window / gpr.height() as f32;
        let cropped = subset_of(&gpr, None, None, Some(40), None).unwrap();
        assert!(
            (cropped.twtt_crop_uniform_ns().unwrap() - (at_zero + 40.0 * step)).abs() < 1e-3,
            "the crop grew"
        );
        assert_eq!(
            cropped.twtt_time_zero_uniform_ns().unwrap(),
            at_zero,
            "time zero did not move: the pulse left when it left"
        );

        let anchor_t0 =
            cropped.twtt_crop_uniform_ns().unwrap() - cropped.twtt_time_zero_uniform_ns().unwrap();
        assert!(
            (anchor_t0 - 40.0 * step).abs() < 1e-3,
            "sample 0 is now 40 samples past time zero, got {anchor_t0}"
        );
    }

    #[test]
    fn a_radargram_nobody_has_zero_corrected_does_not_claim_to_know_time_zero() {
        // Zero is the honest answer, not a guess: its travel times really
        // are measured from whenever the instrument started sampling, and
        // the anchor `t0` that falls out -- crop minus zero -- is the crop,
        // which is right for exactly that reason.
        let gpr = make_gpr_with_first_break(16, 512);
        assert_eq!(gpr.twtt_time_zero_uniform_ns().unwrap(), 0.);
        assert_eq!(gpr.twtt_crop_uniform_ns().unwrap(), 0.);

        let cropped = subset_of(&gpr, None, None, Some(40), None).unwrap();
        assert_eq!(cropped.twtt_time_zero_uniform_ns().unwrap(), 0.);
        assert!(cropped.twtt_crop_uniform_ns().unwrap() > 0.);
    }

    #[test]
    fn the_per_trace_offset_follows_its_traces_through_every_reshaping() {
        // The invariant the whole change rests on: one value per trace,
        // maintained by everything that changes the trace count. A stale
        // vector would be worse than the mean it replaced, since it would
        // attribute one trace's crop to another.
        let mut gpr = make_gpr_with_a_wandering_first_break(64, 512);
        per_trace_max_peak(&mut gpr);
        let before = gpr.twtt_crop_ns().to_vec();

        let cropped = subset_of(&gpr, Some(10), Some(40), None, None).unwrap();
        assert_eq!(cropped.twtt_crop_ns().len(), cropped.width());
        assert_eq!(
            cropped.twtt_crop_ns(),
            &before[10..40],
            "the traces it kept"
        );

        let mut averaged = make_gpr_with_a_wandering_first_break(64, 512);
        per_trace_max_peak(&mut averaged);
        crate::processing::steps::traces::run_average_traces(&mut averaged, 4).unwrap();
        assert_eq!(averaged.twtt_crop_ns().len(), averaged.width());

        let mut thinned = make_gpr_with_a_wandering_first_break(64, 512);
        per_trace_max_peak(&mut thinned);
        crate::processing::steps::traces::run_remove_traces(&mut thinned, &[0, 1, 2]).unwrap();
        assert_eq!(thinned.twtt_crop_ns().len(), thinned.width());
        assert_eq!(thinned.twtt_crop_ns()[0], before[3]);

        // Time zero carries the same invariant through the same operations,
        // and a length mismatch between the two would be a silent
        // misattribution rather than a panic.
        for gpr in [&cropped, &averaged, &thinned] {
            assert_eq!(
                gpr.twtt_time_zero_ns().len(),
                gpr.width(),
                "time zero must stay one per trace too"
            );
        }
    }

    #[test]
    #[serial_test::serial(netcdf)]
    fn both_offsets_reach_the_file_shaped_by_whether_the_traces_agree() {
        // Written and read back as real NetCDF, because both shapes are new
        // to the writer: its `data_vars` loop took 2D arrays and `u8`
        // scalars and silently skipped everything else, so a dataset that
        // looks right proves nothing about the file.
        //
        // One test for both shapes because they are one property -- the
        // shape is the claim -- and because every extra NetCDF file in the
        // suite is another chance to meet #129.
        let dir = tempfile::tempdir().unwrap();

        let mut uniform = make_gpr_with_first_break(48, 512);
        crate::processing::steps::zero_corr::run_zero_corr_legacy(&mut uniform, 1.).unwrap();
        let uniform_expected = uniform.twtt_crop_uniform_ns().unwrap();
        assert!(uniform_expected > 0., "a crop of zero would prove nothing");
        let uniform_path = dir.path().join("uniform.nc");
        uniform.export(&uniform_path).unwrap();

        let mut wandering = make_gpr_with_a_wandering_first_break(48, 512);
        per_trace_max_peak(&mut wandering);
        let wandering_expected = wandering.twtt_crop_ns().to_vec();
        assert!(
            wandering.twtt_crop_uniform_ns().is_none(),
            "the fixture's first break has to wander"
        );
        let wandering_path = dir.path().join("wandering.nc");
        wandering.export(&wandering_path).unwrap();

        let file = netcdf::open(&uniform_path).unwrap();
        for name in ["twtt_crop", "twtt_time_zero"] {
            let var = file
                .variable(name)
                .unwrap_or_else(|| panic!("{name} should be written as a variable"));
            assert_eq!(
                var.dimensions().len(),
                0,
                "{name}: a scalar, not a length-1 array"
            );
            // The crop landed on time zero, so both hold it.
            let found = var.get_value::<f64, _>(()).unwrap();
            assert!(
                (found - uniform_expected as f64).abs() < 1e-6,
                "{name}: {found} != {uniform_expected}"
            );
            assert!(matches!(
                var.attribute("units").unwrap().value().unwrap(),
                netcdf::AttributeValue::Str(u) if u == "ns"
            ));
        }
        drop(file);

        let file = netcdf::open(&wandering_path).unwrap();
        for name in ["twtt_crop", "twtt_time_zero"] {
            let var = file.variable(name).unwrap();
            // `(x)` says "these differ per trace"; a scalar would say one
            // number is true of all of them.
            assert_eq!(var.dimensions().len(), 1, "{name}");
            assert_eq!(var.dimensions()[0].name(), "x", "{name}");
            let found = var.get_values::<f64, _>(..).unwrap();
            assert_eq!(found.len(), wandering_expected.len(), "{name}");
            for (found, expected) in found.iter().zip(wandering_expected.iter()) {
                assert!(
                    (found - *expected as f64).abs() < 1e-6,
                    "{name}: {found} != {expected}"
                );
            }
        }
    }

    #[test]
    #[serial_test::serial(netcdf)]
    fn a_processed_radargram_can_describe_its_own_anchor_axes() {
        // The whole point of #144 read back by #146: an exported radargram
        // carries enough to build the `coordinates.axes` block that lets
        // picks drawn on it be carried onto a later revision.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("line.nc");

        let mut gpr = make_gpr_with_first_break(64, 512);
        crate::processing::steps::zero_corr::run_zero_corr_legacy(&mut gpr, 1.).unwrap();
        gpr.export(&path).unwrap();

        let declared = crate::interp::source::read_axis_declarations(&path).unwrap();
        assert!(
            declared.dt_ns > 0.0,
            "a sample interval from the axis itself"
        );
        assert_eq!(declared.twtt_anchor.as_deref(), Some("twtt"));

        let y = crate::interp::anchors::twtt_axis(
            declared.twtt_anchor.as_deref(),
            &declared.twtt_crop,
            &declared.twtt_time_zero,
            declared.dt_ns,
        )
        .expect("a zero-corrected radargram can say where its axis starts");
        // The crop landed on time zero, so sample 0 is travel time 0.
        assert_eq!(y.t0, Some(0.0));

        let x = crate::interp::anchors::trace_time_axis(&declared.time)
            .expect("the fixture's acquisition times advance");
        let points = x.points.as_ref().unwrap();
        assert!(!points.is_empty());
        assert_eq!(points[0].trace, 0.0);
        assert_eq!(
            points.last().unwrap().trace,
            (gpr.width() - 1) as f64,
            "the last trace is always a tiepoint, so nothing extrapolates"
        );

        assert!(crate::interp::anchors::Axes {
            x: Some(crate::interp::anchors::Axis { anchor: vec![x] }),
            y: Some(crate::interp::anchors::Axis { anchor: vec![y] })
        }
        .is_usable());
    }

    #[test]
    #[serial_test::serial(netcdf)]
    fn a_level_2_export_reads_back_what_the_radargram_declared() {
        // The writer and the reader, across a real file. Level 2 puts these
        // on every row so a point stays self-describing once several
        // radargrams' points are merged, and the value it writes has to be
        // the one this radargram actually declared.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("line.nc");

        let mut gpr = make_gpr_with_first_break(48, 512);
        crate::processing::steps::geometry::run_correct_antenna_separation(
            &mut gpr,
            super::SeparationMethod::Slant,
            crate::tools::SPEED_OF_LIGHT_AIR_M_PER_NS,
        )
        .unwrap();
        gpr.export(&path).unwrap();

        let geometry = crate::interp::source::read_geometry(&path).unwrap();
        assert_eq!(geometry.antenna_separation_effective_m, Some(0.0));
        assert_eq!(
            geometry.twtt_anchor.as_deref(),
            Some("twtt_normal_incidence")
        );
    }

    #[test]
    fn a_radargram_acquired_at_zero_separation_still_declares_plain_twtt() {
        // Nothing was corrected -- `correct_antenna_separation` refuses to
        // run at all here -- so the axis is the recorded one. The two
        // quantities happen to coincide, but claiming the corrected name
        // would let an uncorrected file be re-anchored onto a corrected
        // one, and this is the direction that must fail closed.
        let mut gpr = make_exportable_gpr(32, 128);
        gpr.metadata.antenna_separation = 0.;
        gpr.antenna_separation_effective = 0.;

        crate::processing::steps::geometry::run_correct_antenna_separation(
            &mut gpr,
            super::SeparationMethod::Slant,
            crate::tools::SPEED_OF_LIGHT_AIR_M_PER_NS,
        )
        .unwrap();
        let ds = gpr.export_dataset().unwrap();
        assert_eq!(str_attr(&ds.coords["twtt"].attrs, "anchor_name"), "twtt");
        assert_eq!(f32_attr(&ds, "antenna_separation_effective"), 0.);
    }

    #[test]
    fn depths_keep_the_antenna_separation_after_a_zero_correction() {
        // #261: time zero used to be read as the direct wave's flight time,
        // and a pre-trigger delay of 107 ns (25 MHz on Drønbreen) wiped out
        // a 6.2 m separation entirely. Where time zero sits on the recording
        // clock must not change the geometry.
        let mut gpr = make_dummy_gpr(4, 1024, None);
        gpr.metadata.medium_velocity = 0.168;
        gpr.antenna_separation_effective = 6.2;
        let before = gpr.depths();

        gpr.crop_ns = vec![107.; 4];
        gpr.time_zero_ns = vec![107.; 4];
        let after = gpr.depths();
        assert_eq!(before, after);

        let twtt = gpr.twtt_ns();
        for (time, depth) in twtt.iter().zip(after.iter()) {
            assert_eq!(
                *depth,
                crate::tools::return_time_to_depth(
                    *time,
                    0.168,
                    6.2,
                    crate::tools::SPEED_OF_LIGHT_AIR_M_PER_NS,
                )
            );
        }
        // Near the surface no reflection can have arrived yet, and deeper down
        // the air wave's lead puts depths below `twtt * v / 2`.
        assert_eq!(after[5], 0.);
        assert!(after[200] > twtt[200] * 0.168 / 2.);
    }

    #[test]
    fn selected_traces_keep_everything_that_describes_them() {
        let mut gpr = make_labelled_gpr(10, 5);
        gpr.select_traces(&[7, 2, 3]);

        assert_eq!(gpr.width(), 3);
        assert_eq!(gpr.metadata.last_trace, 3);
        assert_eq!(gpr.data.row(0).to_vec(), vec![7., 2., 3.]);
        assert_eq!(
            gpr.topo_data.as_ref().unwrap().row(0).to_vec(),
            vec![7., 2., 3.]
        );
        assert_eq!(gpr.crop_ns, vec![7., 2., 3.]);
        assert_eq!(gpr.time_zero_ns, vec![107., 102., 103.]);
        let easting = |i: usize| gpr.location.cor_points[i].easting;
        assert_eq!((easting(0), easting(1), easting(2)), (7., 2., 3.));
    }

    #[test]
    fn replaced_traces_must_all_have_one_entry_per_trace() {
        let traces = |width: usize, crops: usize, topo: usize| super::Traces {
            data: ndarray::Array2::zeros((5, width)),
            topo_data: Some(ndarray::Array2::zeros((8, topo))),
            cor_points: make_dummy_gpr(width, 5, Some(1.)).location.cor_points,
            crop_ns: vec![0.; crops],
            time_zero_ns: vec![0.; width],
        };
        let mut gpr = make_labelled_gpr(10, 5);

        let refusal = gpr.replace_traces(traces(4, 3, 4)).unwrap_err();
        assert!(refusal.contains("3 crop offsets for 4"), "{refusal}");
        let refusal = gpr.replace_traces(traces(4, 4, 10)).unwrap_err();
        assert!(
            refusal.contains("10 topographically corrected traces for 4"),
            "{refusal}"
        );
        // Refused means unchanged.
        assert_eq!(gpr.width(), 10);
        assert_eq!(gpr.crop_ns.len(), 10);

        gpr.replace_traces(traces(4, 4, 4)).unwrap();
        assert_eq!(gpr.width(), 4);
        assert_eq!(gpr.metadata.last_trace, 4);
        assert_eq!(gpr.crop_ns.len(), 4);
    }

    #[test]
    fn replacing_samples_keeps_the_traces() {
        let mut gpr = make_labelled_gpr(10, 5);
        let window = gpr.metadata.time_window;

        assert!(gpr.replace_samples(ndarray::Array2::zeros((5, 9))).is_err());
        gpr.replace_samples(ndarray::Array2::zeros((10, 10)))
            .unwrap();
        assert_eq!(gpr.metadata.samples, 10);
        assert_eq!(gpr.metadata.time_window, 2. * window);
    }

    #[test]
    fn cropping_adds_to_each_traces_crop_and_leaves_time_zero() {
        let mut gpr = make_labelled_gpr(3, 10);
        let ns_per_row = gpr.metadata.time_window / 10.;
        let time_zero = gpr.time_zero_ns.clone();

        assert!(gpr
            .crop_samples(ndarray::Array2::zeros((8, 3)), &[2, 2])
            .is_err());
        gpr.crop_samples(ndarray::Array2::zeros((7, 3)), &[1, 2, 3])
            .unwrap();
        assert_eq!(gpr.height(), 7);
        assert_eq!(
            gpr.crop_ns,
            vec![ns_per_row, 1. + 2. * ns_per_row, 2. + 3. * ns_per_row]
        );
        assert_eq!(gpr.time_zero_ns, time_zero);

        assert!(gpr.set_time_zero_ns(vec![0.; 2]).is_err());
        gpr.set_time_zero_ns(vec![1., 2., 3.]).unwrap();
        assert_eq!(gpr.time_zero_ns, vec![1., 2., 3.]);
    }

    #[test]
    #[serial_test::serial(netcdf)]
    fn synthetic_times_are_said_in_the_file_and_survive_a_merge() {
        let dir = tempfile::tempdir().unwrap();
        let attribute = |gpr: &super::GPR, name: &str| {
            let path = dir.path().join(format!("{name}.nc"));
            gpr.export(&path).unwrap();
            let file = netcdf::open(&path).unwrap();
            file.attribute("time_source")
                .map(|a| format!("{:?}", a.value().unwrap()))
        };
        let recorded = make_dummy_gpr(20, 10, Some(1.));
        assert_eq!(attribute(&recorded, "recorded"), None);

        let mut merged = make_dummy_gpr(20, 10, Some(1.));
        let mut synthetic = make_dummy_gpr(20, 10, Some(1.));
        synthetic.location.time_source = super::TimeSource::Synthetic;
        merged.merge(&synthetic).unwrap();
        assert_eq!(merged.location.time_source, super::TimeSource::Synthetic);
        let said = attribute(&merged, "merged").expect("a time_source attribute");
        assert!(said.contains("synthetic"), "{said}");
    }
}
