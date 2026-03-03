use crate::tools;
use ndarray::{Array, Array2};
use num::{Float, FromPrimitive};

pub mod bandpass;
pub mod coordinates;

pub fn abslog<T: Float>(data: &mut Array2<T>) {
    data.mapv_inplace(|v| v.abs());
    let mut minval = T::one();
    let subsampling = ((data.shape()[0] * data.shape()[1]) as f32 * 0.1).max(100.) as usize;
    for quantile in [0.01, 0.05, 0.5, 0.9] {
        let new_min = tools::quantiles(
            data.iter().filter(|v| v >= &&T::zero()),
            &[quantile],
            Some(subsampling),
        )[0];
        if !new_min.is_zero() {
            minval = new_min;
            break;
        }
    }
    data.mapv_inplace(|v| (v + minval).log10());
}

pub fn siglog<T: Float, D: ndarray::Dimension>(data: &mut Array<T, D>, minval_log10: T) {
    data.mapv_inplace(|v| (v.abs().log10() - minval_log10).max(T::zero()) * v.signum());
}

pub fn average_traces<T: Float + FromPrimitive>(
    data: &Array2<T>,
    window: usize,
) -> Result<Array2<T>, String> {
    if window <= 1 {
        return Err(format!("Window size ({window}) needs to be >= 2"));
    }

    let (nrows, ncols) = data.dim();

    if window > ncols {
        return Err(format!(
            "Averaging window ({window}) is larger than the data width ({ncols})"
        ));
    }

    let new_ncols = ncols.div_ceil(window);

    let mut averaged = Array2::<T>::zeros((nrows, new_ncols));

    for i in 0..new_ncols {
        let start = i * window;
        if start >= ncols {
            break;
        }
        let end = (start + window).min(ncols) as isize;

        let view = data.slice_axis(
            ndarray::Axis(1),
            ndarray::Slice::new(start as isize, Some(end), 1),
        );

        let mut dest = averaged.column_mut(i);

        dest.assign(
            &view
                .mean_axis(ndarray::Axis(1))
                .ok_or("Failure in averaging".to_string())?,
        );
    }

    Ok(averaged)
}

pub fn window_subset_vec<T>(mut v: Vec<T>, window: usize) -> Vec<T> {
    assert!(window >= 1, "window must be >= 1");

    let len = v.len();
    if len == 0 {
        return Vec::new();
    }

    let n_windows = len.div_ceil(window);
    let mut new_vec = Vec::with_capacity(n_windows);

    // Compute the indices of the elements we want to keep
    let mut indices = Vec::with_capacity(n_windows);
    for i in 0..n_windows {
        let start = i * window;
        if start >= len {
            break;
        }
        let end = (start + window).min(len);
        let width = end - start;

        let mid_offset = (width - 1) / 2;
        let idx = start + mid_offset;
        indices.push(idx);
    }

    // Remove elements from the end to the start so indices stay valid
    indices.sort_unstable(); // ascending
    indices.reverse(); // process from largest to smallest

    for idx in indices {
        // `remove` moves T out, no clone required
        let item = v.remove(idx);
        new_vec.push(item);
    }

    new_vec.reverse();
    new_vec
}

#[cfg(test)]
mod tests {
    use ndarray::{Array2, AssignElem};

    #[test]
    fn test_abslog() {
        let mut data = Array2::<f32>::from_shape_vec(
            (10, 10),
            (1..101).into_iter().map(|v| v as f32).collect::<Vec<f32>>(),
        )
        .unwrap();
        super::abslog(&mut data);
        let new_minval = *data
            .iter()
            .min_by(|a, b| a.partial_cmp(b).unwrap())
            .unwrap();
        let new_maxval = *data
            .iter()
            .max_by(|a, b| a.partial_cmp(b).unwrap())
            .unwrap();
        assert!(new_minval > 0.2);
        assert!(new_minval < 1.);
        assert!(new_maxval < 2.1);
        assert!(new_maxval > 1.9);
    }

    #[test]
    fn test_siglog() {
        let arr = ndarray::arr1(&[1000_f32, -1000_f32, 0_f32]);
        let mut arr0 = arr.clone();
        super::siglog(&mut arr0, 0.);
        assert_eq!(arr0, ndarray::arr1(&[3., -3., 0.]));
        let mut arr1 = arr.clone();
        arr1[2].assign_elem(0.0001);
        super::siglog(&mut arr1, 0.);
        assert_eq!(arr1, ndarray::arr1(&[3., -3., 0.]));
        let mut arr2 = arr.clone();
        arr2[2].assign_elem(0.1);
        super::siglog(&mut arr2, -2.);
        assert_eq!(arr2, ndarray::arr1(&[5., -5., 1.]));
    }

    #[test]
    fn test_average_traces() {
        let arr = ndarray::arr2(&[[1_f32, 2_f32, 3_f32], [1_f32, 3_f32, 3_f32]]);
        assert_eq!(arr.dim(), (2, 3));

        let avg = super::average_traces(&arr, 2).unwrap();
        assert_eq!(avg.dim(), (2, 2));

        let expected_avg = ndarray::arr2(&[[1.5_f32, 3.], [2., 3.]]);

        assert_eq!((avg - expected_avg).mean(), Some(0.));

        assert_eq!(super::average_traces(&arr, 3).unwrap().dim(), (2, 1));

        assert_eq!(
            super::average_traces(&arr, 4),
            Err("Averaging window (4) is larger than the data width (3)".to_string())
        );

        assert_eq!(
            super::average_traces(&arr, 0),
            Err("Window size (0) needs to be >= 2".to_string())
        );
    }
    #[test]
    fn test_window_subset_vec() {
        // Helper to keep the assertions readable
        fn check(input_len: usize, window: usize, expected: &[usize]) {
            let v = (0..input_len).collect::<Vec<_>>();
            let out = super::window_subset_vec(v, window);
            assert_eq!(out, expected, "len={input_len}, window={window}");
        }

        // 1) Basic odd window, exact multiple: len = 9, window = 3
        // windows: [0,1,2], [3,4,5], [6,7,8] -> mids: 1, 4, 7
        check(9, 3, &[1, 4, 7]);

        // 2) Odd window with remainder: len = 10, window = 3
        // windows: [0,1,2], [3,4,5], [6,7,8], [9] -> mids: 1, 4, 7, 9
        check(10, 3, &[1, 4, 7, 9]);

        // 3) Even window, exact multiple: len = 8, window = 2
        // windows: [0,1], [2,3], [4,5], [6,7] -> left-of-middle: 0, 2, 4, 6
        check(8, 2, &[0, 2, 4, 6]);

        // 4) Even window with remainder: len = 10, window = 4
        // windows: [0,1,2,3], [4,5,6,7], [8,9] -> mids: 1, 5, 8
        check(10, 4, &[1, 5, 8]);

        // 5) Window larger than len: len = 5, window = 10
        // single window [0..5], midpoint index 2
        check(5, 10, &[2]);
    }
}
