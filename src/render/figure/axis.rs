//! Coordinate axes and their ticks for annotated figures (#142).
//!
//! An axis maps a *position* in raster coordinates (a fractional trace or
//! row index) to a coordinate value (metres, nanoseconds, seconds since the
//! epoch). Positions use the viewer's convention: the value stored for index
//! `i` belongs to position `i`, the same place the viewer draws a pick at
//! `(trace, sample) = (i, ...)`. Between indices the value is interpolated
//! linearly; past the last index it is extrapolated from the last two.
//!
//! **Ticks go where their value is, not where an even spacing in pixels
//! would put them.** Distance along a profile is not linear in trace number
//! when the speed varies, so 10 m ticks are unevenly spaced on the page.
//! Resampling the raster onto a uniform distance grid would hide that,
//! and would also change the samples a figure shows; placing ticks at their
//! true positions keeps the radargram as recorded and the labels honest.
//!
//! Ticks are the multiples `anchor + k * spacing` that fall inside the
//! figure's extent, so a 10 m spacing gives 10, 20, 30 wherever the excerpt
//! starts, never 9.87, 19.87, 29.87.

/// The values of one coordinate axis, indexed by raster position.
#[derive(Debug, Clone)]
pub enum AxisValues {
    /// `value(p) = p`: a trace or sample number.
    Index,
    /// `value(p) = offset + p * slope`: the topographic view's elevation,
    /// `E_top - row * dz`, which is linear by construction.
    Linear { offset: f64, slope: f64 },
    /// `value(i) = values[i]`, interpolated between indices: distance,
    /// time, travel time or depth as the file stores them.
    Table(Vec<f64>),
}

impl AxisValues {
    /// The coordinate value at raster position `p`. `NaN` where a tabulated
    /// axis has no finite value to interpolate from.
    pub fn value_at(&self, p: f64) -> f64 {
        match self {
            AxisValues::Index => p,
            AxisValues::Linear { offset, slope } => offset + p * slope,
            AxisValues::Table(values) => interpolate(values, p),
        }
    }

    /// The axis relabelled to count from `zero_at`: `value - zero_at`, or
    /// `zero_at - value` when `descending`. Positions are unchanged, so the
    /// ticks of the relabelled axis are where its labels say.
    pub fn relabelled(self, zero_at: f64, descending: bool) -> Self {
        let sign = if descending { -1.0 } else { 1.0 };
        if zero_at == 0.0 && !descending {
            return self;
        }
        match self {
            AxisValues::Index => AxisValues::Linear {
                offset: -sign * zero_at,
                slope: sign,
            },
            AxisValues::Linear { offset, slope } => AxisValues::Linear {
                offset: sign * (offset - zero_at),
                slope: sign * slope,
            },
            AxisValues::Table(values) => {
                AxisValues::Table(values.into_iter().map(|v| sign * (v - zero_at)).collect())
            }
        }
    }

    /// The raster position where the axis first reaches `value` inside
    /// `[p0, p1]`, or `None` if it does not.
    ///
    /// "First" matters on a tabulated axis that is flat for a while: a
    /// standstill holds distance constant across many traces, and its
    /// tick belongs where the distance was reached, once, not at every
    /// trace that repeats it.
    pub fn position_of(&self, value: f64, p0: f64, p1: f64) -> Option<f64> {
        match self {
            AxisValues::Index => (p0..=p1).contains(&value).then_some(value),
            AxisValues::Linear { offset, slope } => {
                if *slope == 0.0 {
                    return None;
                }
                let p = (value - offset) / slope;
                (p0..=p1).contains(&p).then_some(p)
            }
            AxisValues::Table(_) => {
                let knots = self.knots(p0, p1);
                knots.windows(2).find_map(|pair| {
                    let ((pa, va), (pb, vb)) = (pair[0], pair[1]);
                    if va == value {
                        return Some(pa);
                    }
                    if !va.is_finite() || !vb.is_finite() {
                        return None;
                    }
                    if (va - value) * (vb - value) < 0.0 || vb == value {
                        Some(pa + (value - va) / (vb - va) * (pb - pa))
                    } else {
                        None
                    }
                })
            }
        }
    }

    /// The smallest and largest finite value the axis takes over
    /// `[p0, p1]`, or `None` if it takes none.
    pub fn range(&self, p0: f64, p1: f64) -> Option<(f64, f64)> {
        let values: Vec<f64> = match self {
            AxisValues::Index | AxisValues::Linear { .. } => {
                vec![self.value_at(p0), self.value_at(p1)]
            }
            AxisValues::Table(_) => self.knots(p0, p1).into_iter().map(|(_, v)| v).collect(),
        };
        values
            .into_iter()
            .filter(|v| v.is_finite())
            .fold(None, |acc, v| match acc {
                None => Some((v, v)),
                Some((lo, hi)) => Some((lo.min(v), hi.max(v))),
            })
    }

    /// `(position, value)` at `p0`, every whole index strictly inside, and
    /// `p1`: the piecewise-linear curve the axis follows over the window.
    fn knots(&self, p0: f64, p1: f64) -> Vec<(f64, f64)> {
        let mut knots = vec![(p0, self.value_at(p0))];
        let first = p0.floor() as i64 + 1;
        let last = p1.ceil() as i64 - 1;
        for i in first..=last {
            let p = i as f64;
            if p > p0 && p < p1 {
                knots.push((p, self.value_at(p)));
            }
        }
        knots.push((p1, self.value_at(p1)));
        knots
    }
}

/// Linear interpolation in `values` at fractional index `p`, extrapolating
/// from the nearest two values outside `[0, len - 1]`.
fn interpolate(values: &[f64], p: f64) -> f64 {
    match values.len() {
        0 => f64::NAN,
        1 => values[0],
        n => {
            let i = (p.floor().max(0.0) as usize).min(n - 2);
            let t = p - i as f64;
            // Exactly on an index, that index's value alone, so a missing
            // neighbour does not make a present value NaN.
            if t == 0.0 {
                values[i]
            } else if t == 1.0 {
                values[i + 1]
            } else {
                values[i] + (values[i + 1] - values[i]) * t
            }
        }
    }
}

/// How tick labels are written.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LabelFormat {
    /// A plain number with this many decimals.
    Decimal(usize),
    /// Seconds since 1970-01-01 as a UTC time of day, `HH:MM` when every
    /// tick is a whole minute and `HH:MM:SS` otherwise.
    TimeOfDay { seconds: bool },
}

impl LabelFormat {
    pub fn format(&self, value: f64) -> String {
        match *self {
            LabelFormat::Decimal(decimals) => {
                let text = format!("{value:.decimals$}");
                // `-0` and `-0.0` are what a tiny negative rounding error
                // prints as, and no axis has a use for them.
                if text
                    .trim_start_matches('-')
                    .chars()
                    .all(|c| c == '0' || c == '.')
                {
                    text.trim_start_matches('-').to_string()
                } else {
                    text
                }
            }
            LabelFormat::TimeOfDay { seconds } => {
                let total = value.round().rem_euclid(86_400.0) as i64;
                let (h, m, s) = (total / 3600, (total / 60) % 60, total % 60);
                if seconds {
                    format!("{h:02}:{m:02}:{s:02}")
                } else {
                    format!("{h:02}:{m:02}")
                }
            }
        }
    }
}

/// The fewest decimals (up to 6) that write every `anchor + k * spacing`
/// exactly, so a 0.25 spacing prints `0.25`, `0.50` and a 10 spacing
/// prints `10`, `20`.
pub fn decimals_for(spacing: f64, anchor: f64) -> usize {
    let exact = |x: f64, d: usize| {
        let scale = 10f64.powi(d as i32);
        ((x * scale).round() / scale - x).abs() <= 1e-9 * x.abs().max(1.0)
    };
    (0..=6)
        .find(|&d| exact(spacing, d) && exact(anchor, d))
        .unwrap_or(6)
}

/// A "nice" spacing for a span of `span` units that gives about `target`
/// ticks: the smallest of 1, 2, 2.5 and 5 times a power of ten at least
/// `span / target`.
pub fn nice_spacing(span: f64, target: usize) -> f64 {
    if !(span.is_finite() && span > 0.0) {
        return 1.0;
    }
    let raw = span / target.max(1) as f64;
    let magnitude = 10f64.powf(raw.log10().floor());
    [1.0, 2.0, 2.5, 5.0, 10.0]
        .into_iter()
        .map(|m| m * magnitude)
        .find(|&s| s >= raw * (1.0 - 1e-9))
        .unwrap_or(10.0 * magnitude)
}

/// The time-of-day counterpart of [`nice_spacing`], in seconds: spacings
/// that land on whole seconds, minutes and hours.
pub fn nice_time_spacing(span: f64, target: usize) -> f64 {
    const STEPS: [f64; 18] = [
        1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0, 900.0, 1800.0, 3600.0, 7200.0,
        10800.0, 21600.0, 43200.0, 86400.0,
    ];
    if !(span.is_finite() && span > 0.0) {
        return 60.0;
    }
    let raw = span / target.max(1) as f64;
    STEPS.into_iter().find(|&s| s >= raw).unwrap_or(86400.0)
}

/// Most ticks one axis may have. A spacing that would give more is
/// almost certainly a unit mistake (a 1 m spacing on a 50 km profile),
/// and drawing tens of thousands of overlapping labels helps nobody.
pub const MAX_TICKS: usize = 200;

/// One tick: where it goes and what it says.
#[derive(Debug, Clone, PartialEq)]
pub struct Tick {
    /// Raster position (trace or row).
    pub position: f64,
    pub value: f64,
    pub label: String,
}

/// The ticks at `anchor + k * spacing` that the axis reaches inside the
/// window `[p0, p1]`, in increasing position order.
pub fn ticks(
    axis: &AxisValues,
    p0: f64,
    p1: f64,
    spacing: f64,
    anchor: f64,
    format: LabelFormat,
) -> Result<Vec<Tick>, String> {
    if !(spacing.is_finite() && spacing > 0.0) {
        return Err(format!("A tick spacing must be positive, not {spacing}."));
    }
    if !anchor.is_finite() {
        return Err(format!("A tick anchor must be finite, not {anchor}."));
    }
    let Some((lo, hi)) = axis.range(p0, p1) else {
        return Ok(Vec::new());
    };
    let k0 = ((lo - anchor) / spacing - 1e-9).ceil() as i64;
    let k1 = ((hi - anchor) / spacing + 1e-9).floor() as i64;
    let count = (k1 - k0 + 1).max(0) as usize;
    if count > MAX_TICKS {
        return Err(format!(
            "A tick spacing of {spacing} gives {count} ticks over this extent; at most \
             {MAX_TICKS} are drawn. Use a larger spacing."
        ));
    }
    let mut ticks: Vec<Tick> = (k0..=k1)
        .filter_map(|k| {
            let value = anchor + k as f64 * spacing;
            axis.position_of(value, p0, p1).map(|position| Tick {
                position,
                value,
                label: format.format(value),
            })
        })
        .collect();
    ticks.sort_by(|a, b| a.position.total_cmp(&b.position));
    Ok(ticks)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_relabelled_axis_counts_from_zero_at_in_either_direction() {
        // #496: a 10 km profile surveyed from the terminus, labelled from
        // the head.
        let km = AxisValues::Table(vec![0.0, 4.0, 10.0]).relabelled(10.0, true);
        assert_eq!(km.value_at(0.0), 10.0);
        assert_eq!(km.value_at(1.5), 3.0);
        assert_eq!(km.value_at(2.0), 0.0);
        let ticks = ticks(&km, 0.0, 2.0, 2.0, 0.0, LabelFormat::Decimal(0)).unwrap();
        let labels: Vec<&str> = ticks.iter().map(|t| t.label.as_str()).collect();
        assert_eq!(labels, ["10", "8", "6", "4", "2", "0"]);
        assert_eq!(ticks[1].position, 0.5);

        let traces = AxisValues::Index.relabelled(100.0, true);
        assert_eq!(traces.value_at(30.0), 70.0);
        let shifted = AxisValues::Index.relabelled(100.0, false);
        assert_eq!(shifted.value_at(30.0), -70.0);
        let linear = AxisValues::Linear {
            offset: 500.0,
            slope: -2.0,
        }
        .relabelled(400.0, true);
        assert_eq!(linear.value_at(10.0), -80.0);
        assert!(matches!(
            AxisValues::Index.relabelled(0.0, false),
            AxisValues::Index
        ));
    }

    #[test]
    fn ticks_are_multiples_of_the_spacing_from_the_anchor_not_from_the_extent() {
        // The behaviour #142 asks for by name: a 10 m spacing on an excerpt
        // starting at 9.87 m gives 10, 20, 30, never 9.87, 19.87, 29.87.
        let axis = AxisValues::Linear {
            offset: 9.87,
            slope: 1.0,
        };
        let ticks = ticks(&axis, 0.0, 25.0, 10.0, 0.0, LabelFormat::Decimal(0)).unwrap();
        let values: Vec<f64> = ticks.iter().map(|t| t.value).collect();
        assert_eq!(values, vec![10.0, 20.0, 30.0]);
        assert!((ticks[0].position - 0.13).abs() < 1e-9);
        let labels: Vec<&str> = ticks.iter().map(|t| t.label.as_str()).collect();
        assert_eq!(labels, vec!["10", "20", "30"]);
    }

    #[test]
    fn an_anchor_offsets_every_tick() {
        let ticks = ticks(
            &AxisValues::Index,
            0.0,
            30.0,
            10.0,
            5.0,
            LabelFormat::Decimal(0),
        )
        .unwrap();
        let values: Vec<f64> = ticks.iter().map(|t| t.value).collect();
        assert_eq!(values, vec![5.0, 15.0, 25.0]);
    }

    #[test]
    fn distance_ticks_sit_at_their_true_positions_on_an_uneven_axis() {
        // Ten metres over the first two traces, then one metre per trace:
        // the 10 m tick is at trace 2 and the 20 m tick at trace 12, not
        // evenly spaced on the page.
        let mut distance = vec![0.0, 5.0, 10.0];
        distance.extend((1..=20).map(|i| 10.0 + i as f64));
        let axis = AxisValues::Table(distance);
        let ticks = ticks(&axis, 0.0, 22.0, 10.0, 0.0, LabelFormat::Decimal(0)).unwrap();
        let positions: Vec<f64> = ticks.iter().map(|t| t.position).collect();
        assert_eq!(positions, vec![0.0, 2.0, 12.0, 22.0]);
    }

    #[test]
    fn a_standstill_gives_one_tick_where_its_distance_was_reached() {
        // Traces 2 to 6 are all at 10 m: one tick, at trace 2.
        let axis = AxisValues::Table(vec![0.0, 5.0, 10.0, 10.0, 10.0, 10.0, 10.0, 15.0]);
        let ticks = ticks(&axis, 0.0, 7.0, 10.0, 0.0, LabelFormat::Decimal(0)).unwrap();
        let positions: Vec<f64> = ticks.iter().map(|t| t.position).collect();
        assert_eq!(positions, vec![0.0, 2.0]);
    }

    #[test]
    fn a_decreasing_axis_still_finds_its_ticks() {
        // Elevation decreases down the raster.
        let axis = AxisValues::Linear {
            offset: 100.0,
            slope: -0.5,
        };
        let ticks = ticks(&axis, 0.0, 100.0, 25.0, 0.0, LabelFormat::Decimal(0)).unwrap();
        let pairs: Vec<(f64, f64)> = ticks.iter().map(|t| (t.position, t.value)).collect();
        assert_eq!(pairs, vec![(0.0, 100.0), (50.0, 75.0), (100.0, 50.0)]);
    }

    #[test]
    fn a_fractional_window_interpolates_between_traces() {
        let axis = AxisValues::Table(vec![0.0, 2.0, 4.0, 6.0]);
        assert_eq!(axis.value_at(1.25), 2.5);
        // Past the last trace, the last step is extrapolated.
        assert_eq!(axis.value_at(3.5), 7.0);
        let ticks = ticks(&axis, 0.5, 2.5, 1.0, 0.0, LabelFormat::Decimal(0)).unwrap();
        let positions: Vec<f64> = ticks.iter().map(|t| t.position).collect();
        assert_eq!(positions, vec![0.5, 1.0, 1.5, 2.0, 2.5]);
    }

    #[test]
    fn a_missing_distance_is_skipped_rather_than_ticked() {
        let axis = AxisValues::Table(vec![0.0, f64::NAN, 20.0, 30.0]);
        let ticks = ticks(&axis, 0.0, 3.0, 10.0, 0.0, LabelFormat::Decimal(0)).unwrap();
        let values: Vec<f64> = ticks.iter().map(|t| t.value).collect();
        assert_eq!(values, vec![0.0, 20.0, 30.0]);
    }

    #[test]
    fn too_many_ticks_is_an_error_not_a_wall_of_labels() {
        let err = ticks(
            &AxisValues::Index,
            0.0,
            10_000.0,
            1.0,
            0.0,
            LabelFormat::Decimal(0),
        )
        .unwrap_err();
        assert!(err.contains("larger spacing"), "{err}");
        assert!(ticks(
            &AxisValues::Index,
            0.0,
            1.0,
            0.0,
            0.0,
            LabelFormat::Decimal(0)
        )
        .is_err());
    }

    #[test]
    fn nice_spacings() {
        assert_eq!(nice_spacing(100.0, 5), 20.0);
        assert_eq!(nice_spacing(390.0, 5), 100.0);
        assert_eq!(nice_spacing(11.0, 5), 2.5);
        assert_eq!(nice_spacing(0.9, 5), 0.2);
        assert_eq!(nice_time_spacing(275.0, 5), 60.0);
        assert_eq!(nice_time_spacing(4.0 * 3600.0, 5), 3600.0);
    }

    #[test]
    fn labels_use_only_the_decimals_they_need() {
        assert_eq!(decimals_for(10.0, 0.0), 0);
        assert_eq!(decimals_for(0.25, 0.0), 2);
        assert_eq!(decimals_for(2.5, 0.0), 1);
        assert_eq!(decimals_for(10.0, 0.5), 1);
        assert_eq!(LabelFormat::Decimal(1).format(-0.00001), "0.0");
        assert_eq!(LabelFormat::Decimal(0).format(-20.0), "-20");
    }

    #[test]
    fn time_of_day_labels() {
        // 2024-05-05T18:00:43Z
        let t = 1_714_932_043.0;
        assert_eq!(
            LabelFormat::TimeOfDay { seconds: true }.format(t),
            "18:00:43"
        );
        assert_eq!(
            LabelFormat::TimeOfDay { seconds: false }.format(t - 43.0),
            "18:00"
        );
    }
}
