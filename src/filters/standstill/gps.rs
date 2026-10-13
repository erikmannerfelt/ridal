//! What the positions say about standstills: the GPS side of
//! `remove_standstills(gps_uncertainty=...)`.
//!
//! The radar data alone take two kinds of moving stretch for standstills:
//! where the signal is lost and only the direct wave is left, and where the
//! data are weak or noisy. Both are as coherent from trace to trace as a
//! standstill. They also miss standstills whose traces are only a little more
//! coherent than the moving ones. The positions tell these apart, once their
//! uncertainty is allowed for. Developed on 34 radargrams from Austfonna
//! (800 MHz Malå, 2005 to 2026), whose GPS ranges from centimetres to
//! positions rounded to whole arc-seconds (31 m north-south).
//!
//! Each stretch the radar finds is judged from **its own fixes**
//! ([`Gps::moved`]): the median position of the first third of them against
//! that of the last third. It moved if they are more than [`UNCERTAINTIES`]
//! times the uncertainty apart, at more than [`SLOWEST_SURVEY_SPEED`]. Only
//! its own, because the fixes around a short standstill are the driving
//! before and after it. Medians ignore a wild fix as long as there are three
//! per third; with fewer, the verdict has to hold with any one fix left out.
//! A single jump between two neighbouring fixes that carries most of the
//! movement, much faster than the rest, is taken out first: it is the GPS
//! jumping, not the radar moving. With fewer than [`MIN_FIXES`] fixes, the
//! positions have nothing to say.
//!
//! - A stretch that reached the strength and **did not move** stays as the
//!   radar found it: its edges agree with the positions' to about a second.
//! - One that **moved** keeps only the parts that the positions show standing
//!   still ([`Gps::still_pieces`]), for at least [`Gps::window_s`] each: the
//!   time it takes to move three uncertainties at the slowest survey speed,
//!   so that the positions could have seen it move. That is how a stretch of
//!   lost signal while driving goes, and how a real stop that the radar ran
//!   together with the lost signal beside it stays.
//! - One that did not reach the strength is a standstill all the same if it
//!   did not move and lasts twice the window. The positions vouch for it.
//! - With too few fixes, the radar decides alone.
//!
//! Fixes are found as the traces where the per-trace track bends
//! ([`fixes`]), since positions are interpolated linearly between fixes when
//! a radargram is loaded.
//!
//! The worse the positions, the longer a stretch has to be for them to rule
//! it out or vouch for it: the window is 3 s at 1 m and 30 s at 10 m. So
//! giving a larger uncertainty than the real one makes the step more like
//! the radar alone, and a smaller one makes jumps and drift in the positions
//! look like movement.

/// The slowest the radar is taken to move while surveying, in m/s. Anything
/// slower is standing still, which is what lets a GPS track that wanders
/// slowly through a long stop (0.1 m/s over five minutes at Austfonna in
/// 2005) be read as one.
pub const SLOWEST_SURVEY_SPEED: f64 = 1.;

/// How many times the uncertainty a displacement must exceed to be
/// movement.
pub const UNCERTAINTIES: f64 = 3.;

/// The fewest fixes in a stretch for the positions to judge it.
pub const MIN_FIXES: usize = 5;

/// The fixes in a third that make its median ignore one wild fix.
const ROBUST_THIRD: usize = 3;

/// The shortest window, in seconds and in fix intervals.
const MIN_WINDOW_S: f64 = 2.;
const MIN_WINDOW_INTERVALS: f64 = 2.5;

/// How much faster than the rest of a stretch a jump between two fixes is.
const JUMP_SPEEDUP: f64 = 3.;

/// How far a track may bend, between neighbouring traces, before the middle
/// one is a fix. Interpolated positions are straight to within rounding,
/// which is about a nanometre on projected coordinates and, because times
/// are seconds since 1970, about a tenth of a microsecond on times. A fix
/// bends the times by the difference between two trace intervals, which is
/// a millisecond or more at the trace rates in use.
const BEND_M: f64 = 1e-6;
const BEND_S: f64 = 1e-5;

/// The per-trace positions (projected, in m) and times (s) of a radargram.
#[derive(Debug, Clone, Copy)]
pub struct Track<'a> {
    pub seconds: &'a [f64],
    pub easting: &'a [f64],
    pub northing: &'a [f64],
}

impl Track<'_> {
    fn len(&self) -> usize {
        self.seconds.len()
    }
}

/// The traces where `track` bends, which are its fixes, and its first and
/// last trace.
pub fn fixes(track: &Track) -> Vec<usize> {
    let n = track.len();
    if n < 3 {
        return (0..n).collect();
    }
    let bend = |v: &[f64], j: usize| (v[j + 1] - 2. * v[j] + v[j - 1]).abs();
    let mut out = vec![0];
    out.extend((1..n - 1).filter(|&j| {
        bend(track.easting, j) > BEND_M
            || bend(track.northing, j) > BEND_M
            || bend(track.seconds, j) > BEND_S
    }));
    out.push(n - 1);
    out
}

/// The positions of a radargram, ready to judge stretches of it.
#[derive(Debug, Clone)]
pub struct Gps<'a> {
    track: Track<'a>,
    /// Trace indices of the fixes.
    fixes: Vec<usize>,
    /// The usual time between fixes, in seconds.
    interval_s: f64,
    /// The displacement that is movement, in m.
    threshold_m: f64,
    /// How long a stretch must last for the positions to see it move at the
    /// slowest survey speed, in seconds.
    pub window_s: f64,
}

impl<'a> Gps<'a> {
    /// Prepare `track`, whose positions have the given uncertainty (m). See
    /// the module docs.
    ///
    /// Returns an error when the positions cannot help: too few fixes, times
    /// that are not finite or go backwards, or a track that never moves
    /// further than the uncertainty allows, which is what a radargram
    /// without positions has.
    pub fn new(track: Track<'a>, uncertainty: f64) -> Result<Self, String> {
        if track.seconds.iter().any(|t| !t.is_finite())
            || track.seconds.windows(2).any(|w| w[1] < w[0])
        {
            return Err("the GPS times are not in order".into());
        }
        if track
            .easting
            .iter()
            .chain(track.northing)
            .any(|v| !v.is_finite())
        {
            return Err("some positions are not finite".into());
        }
        let fixes = fixes(&track);
        let intervals: Vec<f64> = fixes
            .windows(2)
            .map(|w| track.seconds[w[1]] - track.seconds[w[0]])
            .filter(|&d| d > 0.)
            .collect();
        if fixes.len() < 2 * MIN_FIXES || intervals.is_empty() {
            return Err(format!(
                "there are only {} fixes, too few to judge movement from",
                fixes.len()
            ));
        }
        let interval_s = median(intervals);
        let threshold_m = UNCERTAINTIES * uncertainty;
        let window_s = MIN_WINDOW_S
            .max(threshold_m / SLOWEST_SURVEY_SPEED)
            .max(MIN_WINDOW_INTERVALS * interval_s);

        let centre = (
            median(fixes.iter().map(|&i| track.easting[i]).collect()),
            median(fixes.iter().map(|&i| track.northing[i]).collect()),
        );
        let away = fixes
            .iter()
            .filter(|&&i| {
                (track.easting[i] - centre.0).hypot(track.northing[i] - centre.1) > threshold_m
            })
            .count();
        if away < MIN_FIXES {
            return Err(format!(
                "the GPS never moves more than {threshold_m} m (three uncertainties)"
            ));
        }
        Ok(Gps {
            track,
            fixes,
            interval_s,
            threshold_m,
            window_s,
        })
    }

    /// The seconds from the first to the last trace of `start..end`.
    pub fn duration_s(&self, start: usize, end: usize) -> f64 {
        if end <= start {
            return 0.;
        }
        self.track.seconds[end - 1] - self.track.seconds[start]
    }

    /// The fixes among traces `start..end`.
    fn fixes_in(&self, start: usize, end: usize) -> &[usize] {
        let first = self.fixes.partition_point(|&i| i < start);
        let last = self.fixes.partition_point(|&i| i < end);
        &self.fixes[first..last]
    }

    /// Whether the fixes among traces `start..end` show the radar moving, or
    /// `None` when there are fewer than [`MIN_FIXES`] of them.
    pub fn moved(&self, start: usize, end: usize) -> Option<bool> {
        self.moved_fixes(self.fixes_in(start, end))
    }

    fn moved_fixes(&self, fixes: &[usize]) -> Option<bool> {
        let n = fixes.len();
        if n < MIN_FIXES {
            return None;
        }
        let moved = self.moved_unchecked(fixes);
        if n / 3 >= ROBUST_THIRD || !moved {
            return Some(moved);
        }
        // Too few for the medians to ignore a wild fix on their own, so the
        // verdict must hold without each of them.
        let mut without = Vec::with_capacity(n - 1);
        Some((0..n).all(|left_out| {
            without.clear();
            without.extend(
                fixes
                    .iter()
                    .enumerate()
                    .filter(|&(i, _)| i != left_out)
                    .map(|(_, &f)| f),
            );
            self.moved_unchecked(&without)
        }))
    }

    /// The first third against the last, after taking out one jump.
    fn moved_unchecked(&self, fixes: &[usize]) -> bool {
        let n = fixes.len();
        let third = (n / 3).max(2);
        let t: Vec<f64> = fixes.iter().map(|&i| self.track.seconds[i]).collect();
        let mut x: Vec<f64> = fixes.iter().map(|&i| self.track.easting[i]).collect();
        let mut y: Vec<f64> = fixes.iter().map(|&i| self.track.northing[i]).collect();
        let test = |x: &[f64], y: &[f64]| {
            let distance = (median(x[n - third..].to_vec()) - median(x[..third].to_vec()))
                .hypot(median(y[n - third..].to_vec()) - median(y[..third].to_vec()));
            let span = (median(t[n - third..].to_vec()) - median(t[..third].to_vec())).max(1e-3);
            (
                distance,
                distance > self.threshold_m && distance / span > SLOWEST_SURVEY_SPEED,
            )
        };
        let (distance, moved) = test(&x, &y);
        if !moved {
            return false;
        }
        let steps: Vec<f64> = (1..n)
            .map(|i| (x[i] - x[i - 1]).hypot(y[i] - y[i - 1]))
            .collect();
        let speeds: Vec<f64> = (1..n)
            .map(|i| steps[i - 1] / (t[i] - t[i - 1]).max(1e-3))
            .collect();
        let jump = (0..steps.len())
            .max_by(|&a, &b| steps[a].total_cmp(&steps[b]))
            .expect("at least two fixes");
        let others = median(
            speeds
                .iter()
                .enumerate()
                .filter(|&(i, _)| i != jump)
                .map(|(_, &v)| v)
                .collect(),
        );
        let is_jump = steps[jump] >= 0.5 * distance
            && t[jump + 1] - t[jump] <= 2. * self.interval_s
            && speeds[jump] > JUMP_SPEEDUP * others.max(SLOWEST_SURVEY_SPEED);
        if !is_jump {
            return true;
        }
        let (dx, dy) = (x[jump + 1] - x[jump], y[jump + 1] - y[jump]);
        for i in jump + 1..n {
            x[i] -= dx;
            y[i] -= dy;
        }
        test(&x, &y).1
    }

    /// The longest parts of traces `start..end` that the positions show
    /// standing still, each lasting at least [`Self::window_s`], as
    /// half-open trace ranges from fix to fix.
    ///
    /// Each grows from a fix for as long as its fixes do not move. Since the
    /// test compares thirds, that can reach about a sixth of its length into
    /// the movement after it, so the fixes at its ends that lead away from
    /// it are trimmed. Fixes that are far but no further than the next one
    /// in stay: that is a jump in the positions, not movement.
    pub fn still_pieces(&self, start: usize, end: usize) -> Vec<(usize, usize)> {
        let fixes = self.fixes_in(start, end);
        let n = fixes.len();
        let mut out = Vec::new();
        let mut i = 0;
        while i + MIN_FIXES <= n {
            let mut longest = None;
            for j in i + MIN_FIXES..=n {
                match self.moved_fixes(&fixes[i..j]) {
                    Some(false) => longest = Some(j),
                    // A few moving verdicts in a row end it; one can be a
                    // wild fix at the end.
                    Some(true) if j - longest.unwrap_or(i + MIN_FIXES) > 2 => break,
                    _ => {}
                }
            }
            let Some(j) = longest else {
                i += 1;
                continue;
            };
            let piece = self.trim(&fixes[i..j]);
            if let (Some(&first), Some(&last)) = (piece.first(), piece.last()) {
                if self.duration_s(first, last + 1) >= self.window_s {
                    out.push((first, last + 1));
                }
            }
            i = j;
        }
        // Where a piece ends one fix early, for a wild fix or a jump, the
        // next starts where it left off; together they are one standstill.
        let mut joined: Vec<(usize, usize)> = Vec::with_capacity(out.len());
        for piece in out {
            match joined.last_mut() {
                Some(last) if self.moved(last.0, piece.1) == Some(false) => last.1 = piece.1,
                _ => joined.push(piece),
            }
        }
        joined
    }

    /// `fixes` without those at either end that lead away from the rest.
    fn trim<'f>(&self, fixes: &'f [usize]) -> &'f [usize] {
        let centre = (
            median(fixes.iter().map(|&i| self.track.easting[i]).collect()),
            median(fixes.iter().map(|&i| self.track.northing[i]).collect()),
        );
        let distance: Vec<f64> = fixes
            .iter()
            .map(|&i| (self.track.easting[i] - centre.0).hypot(self.track.northing[i] - centre.1))
            .collect();
        let uncertainty = self.threshold_m / UNCERTAINTIES;
        let leads_away = |at: usize, inner: usize| {
            distance[at] > self.threshold_m && distance[at] - distance[inner] > uncertainty
        };
        let (mut lo, mut hi) = (0, fixes.len());
        while lo + 1 < hi && leads_away(lo, lo + 1) {
            lo += 1;
        }
        while hi > lo + 1 && leads_away(hi - 1, hi - 2) {
            hi -= 1;
        }
        &fixes[lo..hi]
    }
}

fn median(mut values: Vec<f64>) -> f64 {
    let middle = values.len() / 2;
    *values.select_nth_unstable_by(middle, f64::total_cmp).1
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Seconds since 1970 of the first trace: real magnitudes, so that the
    /// rounding of interpolated times is what it is in a real radargram.
    const START: f64 = 1.7e9;
    pub(crate) const SECONDS_PER_TRACE: f64 = 0.1;
    const SPEED: f64 = 4.;

    /// A deterministic, noise-like value in [-1, 1).
    fn hash(i: usize, seed: u64) -> f64 {
        let mut x = (i as u64)
            .wrapping_mul(0x9E37_79B9_7F4A_7C15)
            .wrapping_add(seed.wrapping_mul(0xC2B2_AE3D_27D4_EB4F));
        x ^= x >> 31;
        x = x.wrapping_mul(0xBF58_476D_1CE4_E5B9);
        x ^= x >> 29;
        (x % 2_000_003) as f64 / 1_000_001.5 - 1.
    }

    /// The per-trace times and positions of a radar driving east at 4 m/s,
    /// except during `stops` (trace ranges), with a fix every second whose
    /// position is off by up to `noise` m, interpolated linearly to the
    /// traces as a radargram is loaded. `fix` may change any fix.
    pub(crate) fn track_with(
        width: usize,
        stops: &[(usize, usize)],
        noise: f64,
        fix: impl Fn(usize, (f64, f64)) -> (f64, f64),
    ) -> (Vec<f64>, Vec<f64>, Vec<f64>) {
        let mut along = vec![0.; width];
        for j in 1..width {
            let stopped = stops.iter().any(|&(a, b)| j > a && j < b);
            along[j] = along[j - 1]
                + if stopped {
                    0.
                } else {
                    SPEED * SECONDS_PER_TRACE
                };
        }
        let every = (1. / SECONDS_PER_TRACE).round() as usize;
        let fixes: Vec<usize> = (0..width).step_by(every).collect();
        let positions: Vec<(f64, f64)> = fixes
            .iter()
            .enumerate()
            .map(|(k, &j)| fix(k, (along[j] + noise * hash(k, 1), noise * hash(k, 2))))
            .collect();
        let mut seconds = vec![0.; width];
        let mut easting = vec![0.; width];
        let mut northing = vec![0.; width];
        for j in 0..width {
            let k = (j / every).min(fixes.len() - 1);
            let (k, next) = if k + 1 < fixes.len() {
                (k, k + 1)
            } else {
                (k - 1, k)
            };
            // Past the last fix, positions are held and times go on.
            let w = (j as f64 - fixes[k] as f64) / (fixes[next] - fixes[k]) as f64;
            let held = w.min(1.);
            let t0 = START + fixes[k] as f64 * SECONDS_PER_TRACE;
            let t1 = START + fixes[next] as f64 * SECONDS_PER_TRACE;
            seconds[j] = t0 + w * (t1 - t0);
            easting[j] = positions[k].0 + held * (positions[next].0 - positions[k].0);
            northing[j] = positions[k].1 + held * (positions[next].1 - positions[k].1);
        }
        (seconds, easting, northing)
    }

    pub(crate) fn track(
        width: usize,
        stops: &[(usize, usize)],
        noise: f64,
    ) -> (Vec<f64>, Vec<f64>, Vec<f64>) {
        track_with(width, stops, noise, |_, p| p)
    }

    fn gps<'a>(t: &'a (Vec<f64>, Vec<f64>, Vec<f64>), uncertainty: f64) -> Gps<'a> {
        Gps::new(
            Track {
                seconds: &t.0,
                easting: &t.1,
                northing: &t.2,
            },
            uncertainty,
        )
        .unwrap()
    }

    #[test]
    fn the_fixes_are_where_the_track_bends() {
        let t = track(400, &[(100, 200)], 0.5);
        let found = fixes(&Track {
            seconds: &t.0,
            easting: &t.1,
            northing: &t.2,
        });
        // One fix every ten traces, plus the last trace.
        let expected: Vec<usize> = (0..400).step_by(10).chain([399]).collect();
        assert_eq!(found, expected);
    }

    #[test]
    fn a_drive_moved_and_a_stop_did_not() {
        let t = track(2000, &[(500, 800)], 0.5);
        let gps = gps(&t, 1.);
        assert_eq!(gps.moved(520, 780), Some(false));
        assert_eq!(gps.moved(1000, 1060), Some(true));
        // A stop with a little driving at either end is still a stop: the
        // edges of the radar and of the positions differ by about a second.
        assert_eq!(gps.moved(490, 810), Some(false));
    }

    #[test]
    fn too_few_fixes_say_nothing() {
        let t = track(2000, &[], 0.5);
        assert_eq!(gps(&t, 1.).moved(1000, 1040), None);
    }

    #[test]
    fn one_wild_fix_does_not_move_a_stop() {
        // A coordinate truncated to a round number, 200 m off, as in the
        // 2011 Austfonna tracks, in a short stop and in a long one.
        for stop in [(500, 570), (500, 800)] {
            let wild = stop.0 / 10 + 3;
            let t = track_with(2000, &[stop], 0.5, |k, (x, y)| {
                if k == wild {
                    (x + 200., y)
                } else {
                    (x, y)
                }
            });
            assert_eq!(gps(&t, 1.).moved(stop.0, stop.1), Some(false), "{stop:?}");
        }
    }

    #[test]
    fn a_jump_in_the_positions_is_not_movement() {
        // Rounded to whole arc-seconds, a stopped track can step 31 m from
        // one fix to the next and stay there.
        let t = track_with(2000, &[(500, 800)], 0.5, |k, (x, y)| {
            if k >= 63 {
                (x, y + 31.)
            } else {
                (x, y)
            }
        });
        let gps = gps(&t, 1.);
        assert_eq!(gps.moved(500, 800), Some(false));
        let pieces = gps.still_pieces(500, 800);
        let covered: usize = pieces.iter().map(|(a, b)| b - a).sum();
        assert!(covered >= 270, "{pieces:?}");
    }

    #[test]
    fn a_slowly_wandering_track_is_standing_still() {
        // 0.1 m/s for five minutes: 30 m, far more than the uncertainty.
        let t = track_with(4000, &[(500, 3500)], 0.5, |k, (x, y)| {
            if (50..350).contains(&k) {
                (x + 0.1 * (k - 50) as f64, y)
            } else {
                (x, y)
            }
        });
        assert_eq!(gps(&t, 1.).moved(500, 3500), Some(false));
    }

    #[test]
    fn the_still_part_of_a_moving_stretch_is_found() {
        // Driving 600..1000, standing 1000..1300: what the radar runs
        // together when the signal is lost just before a stop.
        let t = track(2000, &[(1000, 1300)], 0.5);
        let gps = gps(&t, 1.);
        assert_eq!(gps.moved(600, 1300), Some(true));
        let pieces = gps.still_pieces(600, 1300);
        assert_eq!(pieces.len(), 1, "{pieces:?}");
        let (start, end) = pieces[0];
        // Fixes are ten traces apart.
        assert!(
            start.abs_diff(1000) <= 20 && end.abs_diff(1300) <= 20,
            "{pieces:?}"
        );
        assert!(gps.still_pieces(1400, 1900).is_empty());
    }

    #[test]
    fn a_larger_uncertainty_needs_longer_stretches() {
        let t = track(2000, &[], 0.5);
        // Six seconds of driving at 4 m/s: 16 m between the outer thirds.
        assert_eq!(gps(&t, 1.).moved(1000, 1060), Some(true));
        assert_eq!(gps(&t, 10.).moved(1000, 1060), Some(false));
        assert_eq!(gps(&t, 10.).window_s, 30.);
        // But not so long that a stop is judged from the driving around it.
        let t = track(2000, &[(1000, 1150)], 0.5);
        assert_eq!(gps(&t, 10.).moved(1000, 1150), Some(false));
    }

    #[test]
    fn a_track_that_never_moves_is_refused() {
        let t = track(2000, &[(0, 2000)], 0.5);
        let error = Gps::new(
            Track {
                seconds: &t.0,
                easting: &t.1,
                northing: &t.2,
            },
            1.,
        )
        .unwrap_err();
        assert!(error.contains("never moves"), "{error}");
    }

    #[test]
    fn times_that_go_backwards_are_refused() {
        let mut t = track(2000, &[], 0.5);
        t.0[1000] = t.0[0];
        let error = Gps::new(
            Track {
                seconds: &t.0,
                easting: &t.1,
                northing: &t.2,
            },
            1.,
        )
        .unwrap_err();
        assert!(error.contains("not in order"), "{error}");
    }
}
