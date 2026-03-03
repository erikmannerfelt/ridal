use crate::gpr::CorPoint;

#[derive(Debug, Clone)]
struct TrackGeometry {
    s: Vec<f64>,
    normals: Vec<[f64; 2]>,
}

fn build_track_geometry(points: &[CorPoint]) -> TrackGeometry {
    assert!(points.len() >= 2);

    let n = points.len();
    let mut s = vec![0.0; n];
    let mut tangents = vec![[0.0, 0.0]; n];
    let mut normals = vec![[0.0, 0.0]; n];

    let mut seg_dirs = vec![[0.0, 0.0]; n - 1];
    let mut seg_len = vec![0.0; n - 1];

    for i in 0..(n - 1) {
        let dx = points[i + 1].easting - points[i].easting;
        let dy = points[i + 1].northing - points[i].northing;
        let len = (dx * dx + dy * dy).sqrt();
        seg_len[i] = len;

        if len > 0.0 {
            seg_dirs[i] = [dx / len, dy / len];
        } else if i > 0 {
            // zero-length segment: inherit previous direction
            seg_dirs[i] = seg_dirs[i - 1];
        } else {
            // first segment and zero-length: leave as [0,0] (degenerate case)
            seg_dirs[i] = [0.0, 0.0];
        }

        s[i + 1] = s[i] + len;
    }

    // Tangents
    for i in 0..n {
        let t = if i == 0 {
            seg_dirs[0]
        } else if i == n - 1 {
            seg_dirs[n - 2]
        } else {
            let [ux1, uy1] = seg_dirs[i - 1];
            let [ux2, uy2] = seg_dirs[i];
            let mut tx = ux1 + ux2;
            let mut ty = uy1 + uy2;
            let norm = (tx * tx + ty * ty).sqrt();
            if norm > 0.0 {
                tx /= norm;
                ty /= norm;
            }
            [tx, ty]
        };

        tangents[i] = t;
        normals[i] = [-t[1], t[0]];
    }

    TrackGeometry { s, normals }
}

fn interpolate_xy_along_s(points: &[CorPoint], geom: &TrackGeometry, s_query: f64) -> (f64, f64) {
    let n = points.len();
    let s = &geom.s;

    if s_query <= s[0] {
        return (points[0].easting, points[0].northing);
    }
    if s_query >= s[n - 1] {
        return (points[n - 1].easting, points[n - 1].northing);
    }

    // binary search for segment
    let idx = match s.binary_search_by(|val| val.partial_cmp(&s_query).unwrap()) {
        Ok(i) => i,
        Err(i) => i - 1, // s[idx] < s_query < s[idx+1]
    };

    let s0 = s[idx];
    let s1 = s[idx + 1];
    let t = (s_query - s0) / (s1 - s0);

    let p0 = &points[idx];
    let p1 = &points[idx + 1];

    let x = p0.easting + t * (p1.easting - p0.easting);
    let y = p0.northing + t * (p1.northing - p0.northing);

    (x, y)
}

pub fn shift_coordinates(
    points: &[CorPoint],
    delta_s: f64,
    altitude_offset: f64,
    lateral_offset: f64,
) -> Vec<CorPoint> {
    assert!(points.len() >= 2);
    let geom = build_track_geometry(points);

    // First, compute the shifted-along-track positions
    let mut shifted_xy: Vec<(f64, f64)> = Vec::with_capacity(points.len());
    for (i, _) in points.iter().enumerate() {
        let s_i = geom.s[i] + delta_s;
        let (x, y) = interpolate_xy_along_s(points, &geom, s_i);
        shifted_xy.push((x, y));
    }

    // Build geometry of the shifted track (for accurate normals at the shifted location)
    let shifted_points: Vec<CorPoint> = points
        .iter()
        .zip(shifted_xy.iter())
        .map(|(p, &(x, y))| CorPoint {
            trace_n: p.trace_n,
            time_seconds: p.time_seconds,
            easting: x,
            northing: y,
            altitude: p.altitude, // altitude offset later
        })
        .collect();

    let shifted_geom = build_track_geometry(&shifted_points);

    // Apply lateral offset using normals of the shifted track
    shifted_points
        .into_iter()
        .enumerate()
        .map(|(i, p)| {
            let n = shifted_geom.normals[i];
            CorPoint {
                trace_n: p.trace_n,
                time_seconds: p.time_seconds,
                easting: p.easting + lateral_offset * n[0],
                northing: p.northing + lateral_offset * n[1],
                altitude: p.altitude + altitude_offset,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx_eq(a: f64, b: f64, eps: f64) {
        assert!(
            (a - b).abs() <= eps,
            "expected {a} ≈ {b} (|diff| = {} > {eps})",
            (a - b).abs()
        );
    }

    fn make_simple_horizontal_track(n: usize) -> Vec<CorPoint> {
        // Track: points at (0,0), (1,0), ..., (n-1, 0)
        // altitude = 100.0 for all
        (0..n)
            .map(|i| CorPoint {
                trace_n: i as u32,
                time_seconds: i as f64,
                easting: i as f64,
                northing: 0.0,
                altitude: 100.0,
            })
            .collect()
    }

    #[test]
    fn test_shift_along_no_lateral_no_altitude() {
        let points = make_simple_horizontal_track(11); // x = 0..10

        // Shift by +2 m along the track (to the right in +x)
        let shifted = shift_coordinates(&points, 2.0, 0.0, 0.0);

        // Internal points: point i should move from x = i to x = i + 2
        // with clamping at the end.
        let eps = 1e-9;

        // First point: s_0 = 0, s_0 + 2 = 2 => should be at x=2
        approx_eq(shifted[0].easting, 2.0, eps);
        approx_eq(shifted[0].northing, 0.0, eps);

        // Middle point: i = 4, s_4 = 4, s_4 + 2 = 6 => x=6
        approx_eq(shifted[4].easting, 6.0, eps);
        approx_eq(shifted[4].northing, 0.0, eps);

        // Last point: i=10, s_10 = 10, s_10+2 = 12, beyond end => clamped at x=10
        approx_eq(shifted[10].easting, 10.0, eps);
        approx_eq(shifted[10].northing, 0.0, eps);

        // Altitude unchanged
        for i in 0..points.len() {
            approx_eq(shifted[i].altitude, points[i].altitude, eps);
        }
    }

    #[test]
    fn test_shift_along_negative_with_clamping() {
        let points = make_simple_horizontal_track(11); // x = 0..10

        // Shift by -3 m along the track (to the left / backward)
        let shifted = shift_coordinates(&points, -3.0, 0.0, 0.0);
        let eps = 1e-9;

        // First point: s_0 = 0, s_0 - 3 = -3 => clamped at s=0 => x=0
        approx_eq(shifted[0].easting, 0.0, eps);
        approx_eq(shifted[0].northing, 0.0, eps);

        // Middle point: i=5, s_5 = 5, s_5 - 3 = 2 => x=2
        approx_eq(shifted[5].easting, 2.0, eps);
        approx_eq(shifted[5].northing, 0.0, eps);

        // Last point: i=10, s_10=10, s_10-3=7 => x=7
        approx_eq(shifted[10].easting, 7.0, eps);
        approx_eq(shifted[10].northing, 0.0, eps);
    }

    #[test]
    fn test_lateral_offset_left() {
        let points = make_simple_horizontal_track(11); // x=0..10, y=0
                                                       // For a horizontal track in +x direction:
                                                       //   tangent = (1,0), left normal = (0,1)
                                                       // Lateral offset +5 should move points to y=+5
        let shifted = shift_coordinates(&points, 0.0, 0., 5.);
        let eps = 1e-9;

        for (i, p) in shifted.iter().enumerate() {
            approx_eq(p.easting, i as f64, eps); // along-track unchanged
            approx_eq(p.northing, 5.0, eps); // all moved upward
        }
    }

    #[test]
    fn test_lateral_offset_right() {
        let points = make_simple_horizontal_track(11);
        // lateral_offset = -3 => move right (down in y)
        let shifted = shift_coordinates(&points, 0.0, 0.0, -3.);
        let eps = 1e-9;

        for (i, p) in shifted.iter().enumerate() {
            approx_eq(p.easting, i as f64, eps);
            approx_eq(p.northing, -3.0, eps);
        }
    }

    #[test]
    fn test_altitude_offset() {
        let points = make_simple_horizontal_track(11);

        // No spatial shift, altitude +10
        let shifted = shift_coordinates(&points, 0.0, 10., 0.0);
        let eps = 1e-9;

        for (orig, new) in points.iter().zip(shifted.iter()) {
            approx_eq(new.easting, orig.easting, eps);
            approx_eq(new.northing, orig.northing, eps);
            approx_eq(new.altitude, orig.altitude + 10.0, eps);
        }
    }

    #[test]
    fn test_combined_along_and_lateral() {
        let points = make_simple_horizontal_track(11);

        // Shift +2 m along, +4 m left
        let shifted = shift_coordinates(&points, 2.0, 0.0, 4.0);
        let eps = 1e-9;

        // Point i originally at (i, 0).
        // After along-track shift, should be at x = clamp(i + 2, 0..10), y=0.
        // After lateral +4, y=4.
        for (i, p) in shifted.iter().enumerate() {
            println!("{i}");
            let mut expected_x = i as f64 + 2.0;
            if expected_x < 0.0 {
                expected_x = 0.0;
            }
            if expected_x > 10.0 {
                expected_x = 10.0;
            }
            approx_eq(p.easting, expected_x, eps);
            approx_eq(p.northing, 4.0, eps);
        }
    }
}
