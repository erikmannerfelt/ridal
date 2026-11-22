//! Reading `data`'s storage chunks without the NetCDF lock (#577).
//!
//! netcdf-c and HDF5 are not thread-safe, so every call into them holds
//! `netcdf_sys::libnetcdf_lock`, and a read decompresses its chunks while
//! holding it: with several radargrams rendering at once, the lock was busy
//! 93-97 % of the time and was the ceiling on how fast the server could go
//! (#577). Nothing about inflating a chunk needs HDF5, though. At open, under
//! the lock, HDF5 says where each chunk is stored in the file; after that a
//! read is a `pread`, an inflate and an unshuffle, none of which touch the
//! library.
//!
//! Only the layout Ridal writes takes this path: a 2-D chunked
//! little-endian `f32` variable whose filters are exactly shuffle then
//! deflate. Anything else -- an older contiguous file, another filter, a
//! file another tool rewrote -- makes [`RawChunks::open`] return `None`, and
//! the caller reads through netcdf as before. So does a window touching a
//! chunk that was never written, whose values are HDF5's fill value.

use std::path::Path;
use std::sync::{Arc, Mutex};

use hdf5_sys::h5::{haddr_t, hsize_t};
use hdf5_sys::h5i::hid_t;
use ndarray::Array2;

/// Where one chunk's compressed bytes are in the file.
#[derive(Clone, Copy)]
struct Stored {
    addr: u64,
    size: usize,
}

/// A chunk, inflated and unshuffled: `chunk.0 * chunk.1` values, row-major,
/// including the padding HDF5 stores past the edge of the array.
type Decoded = Arc<[f32]>;

pub(super) struct RawChunks {
    file: std::fs::File,
    shape: (usize, usize),
    chunk: (usize, usize),
    /// Chunks per row of the chunk grid.
    grid_cols: usize,
    /// Row-major over the chunk grid; `None` for a chunk with no storage.
    index: Vec<Option<Stored>>,
    /// The most recently used decoded chunks, most recent last.
    ///
    /// Stands in for netcdf-c's chunk cache, which this path bypasses: an
    /// overview band shorter than a chunk, or two amplitude-sample runs in
    /// one chunk column, would otherwise inflate the same chunk again. The
    /// same budget as that cache ([`super::CHUNK_CACHE_BYTES`]), so a file
    /// read this way holds no more than it did. Locked only to look up or
    /// insert, never while inflating.
    cache: Mutex<Vec<(usize, Decoded)>>,
    cache_capacity: usize,
}

/// An HDF5 id closed by `close` when dropped. Dropped under the NetCDF lock,
/// since [`RawChunks::open`] holds it throughout.
struct Id(hid_t, unsafe extern "C" fn(hid_t) -> hdf5_sys::h5::herr_t);

impl Id {
    fn new(id: hid_t, close: unsafe extern "C" fn(hid_t) -> hdf5_sys::h5::herr_t) -> Option<Self> {
        (id >= 0).then_some(Self(id, close))
    }
}

impl Drop for Id {
    fn drop(&mut self) {
        // SAFETY: a valid id of the kind `self.1` closes, closed once.
        unsafe {
            (self.1)(self.0);
        }
    }
}

impl RawChunks {
    /// The chunk index of `path`'s `data` variable, or `None` when its
    /// layout is not one this path reads.
    pub(super) fn open(path: &Path) -> Option<Self> {
        let (shape, chunk, index) = {
            let _guard = netcdf_sys::libnetcdf_lock.lock();
            // SAFETY: every id is checked before use and closed by `Id`;
            // every out-pointer points at a local of the right size.
            unsafe { read_index(path)? }
        };
        let file = std::fs::File::open(path).ok()?;
        let grid_cols = shape.1.div_ceil(chunk.1);
        let chunk_bytes = chunk.0 * chunk.1 * std::mem::size_of::<f32>();
        Some(Self {
            file,
            shape,
            chunk,
            grid_cols,
            index,
            cache: Mutex::new(Vec::new()),
            cache_capacity: (super::CHUNK_CACHE_BYTES / chunk_bytes).max(1),
        })
    }

    /// The window `[row0, row1) x [col0, col1)`, already clamped to the
    /// array by the caller, or `None` when it touches a chunk with no
    /// storage.
    pub(super) fn read_window(
        &self,
        row0: usize,
        row1: usize,
        col0: usize,
        col1: usize,
    ) -> Result<Option<Array2<f32>>, String> {
        debug_assert!(row0 < row1 && row1 <= self.shape.0);
        debug_assert!(col0 < col1 && col1 <= self.shape.1);
        let (ch, cw) = self.chunk;
        let width = col1 - col0;
        let mut out = vec![0f32; (row1 - row0) * width];
        for gy in row0 / ch..=(row1 - 1) / ch {
            for gx in col0 / cw..=(col1 - 1) / cw {
                let Some(chunk) = self.chunk(gy * self.grid_cols + gx)? else {
                    return Ok(None);
                };
                let (y0, x0) = (gy * ch, gx * cw);
                let (c0, c1) = (col0.max(x0), col1.min(x0 + cw));
                for r in row0.max(y0)..row1.min(y0 + ch) {
                    let src = (r - y0) * cw + (c0 - x0);
                    let dst = (r - row0) * width + (c0 - col0);
                    out[dst..dst + (c1 - c0)].copy_from_slice(&chunk[src..src + (c1 - c0)]);
                }
            }
        }
        Array2::from_shape_vec((row1 - row0, width), out)
            .map(Some)
            .map_err(|e| format!("Unexpected window shape: {e}"))
    }

    /// Chunk `i` of the grid, from the cache or the file.
    fn chunk(&self, i: usize) -> Result<Option<Decoded>, String> {
        {
            let mut cache = self.cache.lock().map_err(|_| "chunk cache poisoned")?;
            if let Some(at) = cache.iter().position(|(k, _)| *k == i) {
                let entry = cache.remove(at);
                let chunk = Arc::clone(&entry.1);
                cache.push(entry);
                return Ok(Some(chunk));
            }
        }
        let Some(stored) = self.index[i] else {
            return Ok(None);
        };
        let mut compressed = vec![0u8; stored.size];
        read_at(&self.file, &mut compressed, stored.addr)
            .map_err(|e| format!("Failed to read a chunk of 'data': {e}"))?;
        let chunk: Decoded = decode(&compressed, self.chunk.0 * self.chunk.1)?.into();
        let mut cache = self.cache.lock().map_err(|_| "chunk cache poisoned")?;
        if !cache.iter().any(|(k, _)| *k == i) {
            if cache.len() >= self.cache_capacity {
                cache.remove(0);
            }
            cache.push((i, Arc::clone(&chunk)));
        }
        Ok(Some(chunk))
    }
}

/// Inflate a zlib stream of shuffled little-endian `f32`s and unshuffle it.
///
/// HDF5's shuffle filter stores byte 0 of every value, then byte 1 of every
/// value, and so on, which is what lets deflate find the runs in slowly
/// varying floats.
fn decode(compressed: &[u8], n: usize) -> Result<Vec<f32>, String> {
    let mut shuffled = vec![0u8; n * 4];
    let mut inflate = flate2::Decompress::new(true);
    let status = inflate
        .decompress(compressed, &mut shuffled, flate2::FlushDecompress::Finish)
        .map_err(|e| format!("Failed to inflate a chunk of 'data': {e}"))?;
    if status != flate2::Status::StreamEnd || inflate.total_out() as usize != shuffled.len() {
        return Err(format!(
            "A chunk of 'data' inflated to {} bytes, expected {}",
            inflate.total_out(),
            shuffled.len()
        ));
    }
    let (b0, rest) = shuffled.split_at(n);
    let (b1, rest) = rest.split_at(n);
    let (b2, b3) = rest.split_at(n);
    Ok((0..n)
        .map(|i| f32::from_le_bytes([b0[i], b1[i], b2[i], b3[i]]))
        .collect())
}

#[cfg(unix)]
fn read_at(file: &std::fs::File, buf: &mut [u8], offset: u64) -> std::io::Result<()> {
    std::os::unix::fs::FileExt::read_exact_at(file, buf, offset)
}

#[cfg(windows)]
fn read_at(file: &std::fs::File, mut buf: &mut [u8], mut offset: u64) -> std::io::Result<()> {
    // `seek_read` moves the handle's cursor, which nothing else here uses,
    // and is safe to call from several threads at once.
    while !buf.is_empty() {
        match std::os::windows::fs::FileExt::seek_read(file, buf, offset)? {
            0 => return Err(std::io::ErrorKind::UnexpectedEof.into()),
            n => {
                buf = &mut buf[n..];
                offset += n as u64;
            }
        }
    }
    Ok(())
}

/// Shape, chunk shape and chunk index of `path`'s `data`, when its layout is
/// the one this module reads.
///
/// # Safety
///
/// Must be called with `libnetcdf_lock` held.
unsafe fn read_index(path: &Path) -> Option<((usize, usize), (usize, usize), Vec<Option<Stored>>)> {
    use hdf5_sys::{h5d, h5f, h5p, h5s, h5t, h5z};

    let c_path = std::ffi::CString::new(path.to_string_lossy().as_bytes()).ok()?;
    hdf5_sys::h5::H5open();
    let file = Id::new(
        h5f::H5Fopen(c_path.as_ptr(), h5f::H5F_ACC_RDONLY, h5p::H5P_DEFAULT),
        h5f::H5Fclose,
    )?;
    // Chunk addresses are relative to the end of a user block. netcdf-c
    // never writes one; rather than offset by it, leave such a file to it.
    let fcpl = Id::new(h5f::H5Fget_create_plist(file.0), h5p::H5Pclose)?;
    let mut userblock: hsize_t = 0;
    if h5p::H5Pget_userblock(fcpl.0, &mut userblock) < 0 || userblock != 0 {
        return None;
    }
    let dset = Id::new(
        h5d::H5Dopen2(file.0, c"data".as_ptr(), h5p::H5P_DEFAULT),
        h5d::H5Dclose,
    )?;

    let dtype = Id::new(h5d::H5Dget_type(dset.0), h5t::H5Tclose)?;
    if h5t::H5Tequal(dtype.0, *h5t::H5T_IEEE_F32LE) <= 0 {
        return None;
    }

    let space = Id::new(h5d::H5Dget_space(dset.0), h5s::H5Sclose)?;
    let mut dims = [0 as hsize_t; 2];
    if h5s::H5Sget_simple_extent_ndims(space.0) != 2
        || h5s::H5Sget_simple_extent_dims(space.0, dims.as_mut_ptr(), std::ptr::null_mut()) != 2
    {
        return None;
    }
    let shape = (dims[0] as usize, dims[1] as usize);

    let dcpl = Id::new(h5d::H5Dget_create_plist(dset.0), h5p::H5Pclose)?;
    if h5p::H5Pget_layout(dcpl.0) != h5d::H5D_layout_t::H5D_CHUNKED {
        return None;
    }
    let mut chunk_dims = [0 as hsize_t; 2];
    if h5p::H5Pget_chunk(dcpl.0, 2, chunk_dims.as_mut_ptr()) != 2 {
        return None;
    }
    let chunk = (chunk_dims[0] as usize, chunk_dims[1] as usize);
    if chunk.0 == 0 || chunk.1 == 0 {
        return None;
    }

    if h5p::H5Pget_nfilters(dcpl.0) != 2 {
        return None;
    }
    for (i, expected) in [h5z::H5Z_FILTER_SHUFFLE, h5z::H5Z_FILTER_DEFLATE]
        .into_iter()
        .enumerate()
    {
        let mut flags = 0;
        let mut n_values: usize = 4;
        let mut values = [0u32; 4];
        let mut config = 0;
        let filter = h5p::H5Pget_filter2(
            dcpl.0,
            i as u32,
            &mut flags,
            &mut n_values,
            values.as_mut_ptr(),
            0,
            std::ptr::null_mut(),
            &mut config,
        );
        if filter != expected {
            return None;
        }
        // The shuffle's element size, recorded when the dataset was made.
        if filter == h5z::H5Z_FILTER_SHUFFLE && n_values >= 1 && values[0] != 4 {
            return None;
        }
    }

    let grid = (shape.0.div_ceil(chunk.0), shape.1.div_ceil(chunk.1));
    let mut index = Vec::with_capacity(grid.0 * grid.1);
    for gy in 0..grid.0 {
        for gx in 0..grid.1 {
            let offset = [(gy * chunk.0) as hsize_t, (gx * chunk.1) as hsize_t];
            let mut mask = 0u32;
            let mut addr: haddr_t = 0;
            let mut size: hsize_t = 0;
            if h5d::H5Dget_chunk_info_by_coord(
                dset.0,
                offset.as_ptr(),
                &mut mask,
                &mut addr,
                &mut size,
            ) < 0
            {
                return None;
            }
            // A chunk written with a filter skipped is stored differently
            // from the rest; read the whole file the old way.
            if mask != 0 {
                return None;
            }
            index.push(
                (addr != hdf5_sys::h5::HADDR_UNDEF && size > 0).then_some(Stored {
                    addr,
                    size: size as usize,
                }),
            );
        }
    }
    Some((shape, chunk, index))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A file with one `data` variable of `shape`, written by netcdf as
    /// Ridal writes it unless `layout` says otherwise.
    fn write(path: &Path, shape: (usize, usize), layout: Layout) -> Array2<f32> {
        let data = Array2::from_shape_fn(shape, |(r, c)| {
            // Varying in both directions and in sign, so a transposed or
            // shifted read cannot pass by accident.
            ((r * 7919 + c * 104729) % 1000) as f32 / 37.0 - 13.5
        });
        let mut file = netcdf::create(path).unwrap();
        file.add_dimension("y", shape.0).unwrap();
        file.add_dimension("x", shape.1).unwrap();
        match layout {
            Layout::Double => {
                let mut var = file.add_variable::<f64>("data", &["y", "x"]).unwrap();
                var.set_compression(5, true).unwrap();
                var.set_chunking(&[64, 64]).unwrap();
                var.put(data.mapv(f64::from).view(), ..).unwrap();
            }
            _ => {
                let mut var = file.add_variable::<f32>("data", &["y", "x"]).unwrap();
                match layout {
                    Layout::Ridal(c) => {
                        var.set_compression(5, true).unwrap();
                        var.set_chunking(&[c, c]).unwrap();
                    }
                    Layout::NoShuffle => {
                        var.set_compression(5, false).unwrap();
                        var.set_chunking(&[64, 64]).unwrap();
                    }
                    Layout::Contiguous | Layout::Double => {}
                    Layout::PartlyWritten => {
                        var.set_compression(5, true).unwrap();
                        var.set_chunking(&[64, 64]).unwrap();
                        // Only the first 64 rows: the chunks below are
                        // never written.
                        var.put(data.slice(ndarray::s![..64, ..]), (..64, ..))
                            .unwrap();
                        return data;
                    }
                }
                var.put(data.view(), ..).unwrap();
            }
        }
        data
    }

    #[derive(Clone, Copy)]
    enum Layout {
        Ridal(usize),
        NoShuffle,
        Contiguous,
        Double,
        PartlyWritten,
    }

    fn open(path: &Path) -> Option<RawChunks> {
        RawChunks::open(path)
    }

    #[test]
    #[serial_test::serial(netcdf)]
    fn every_window_reads_what_netcdf_reads() {
        // Chunk edges, the partial last row and column of chunks, single
        // rows and columns, windows wider than a chunk, and the whole array.
        let dir = tempfile::tempdir().unwrap();
        for chunk in [64, 256] {
            let path = dir.path().join(format!("c{chunk}.nc"));
            let data = write(&path, (300, 520), Layout::Ridal(chunk));
            let raw = open(&path).expect("Ridal's own layout takes the raw path");
            assert_eq!(raw.chunk, (chunk, chunk));
            for (r0, r1, c0, c1) in [
                (0, 300, 0, 520),
                (0, 1, 0, 1),
                (299, 300, 519, 520),
                (63, 65, 63, 65),
                (10, 290, 255, 257),
                (64, 128, 128, 192),
                (5, 6, 0, 520),
                (0, 300, 517, 518),
                (100, 299, 3, 511),
            ] {
                let got = raw.read_window(r0, r1, c0, c1).unwrap().unwrap();
                let want = data.slice(ndarray::s![r0..r1, c0..c1]);
                assert_eq!(got.shape(), want.shape(), "window {r0}..{r1} x {c0}..{c1}");
                assert!(
                    got.iter()
                        .zip(want.iter())
                        .all(|(a, b)| a.to_bits() == b.to_bits()),
                    "window {r0}..{r1} x {c0}..{c1} differs (chunk {chunk})"
                );
            }
        }
    }

    #[test]
    #[serial_test::serial(netcdf)]
    fn a_layout_other_than_ridals_is_left_to_netcdf() {
        let dir = tempfile::tempdir().unwrap();
        for (name, layout) in [
            ("no-shuffle", Layout::NoShuffle),
            ("contiguous", Layout::Contiguous),
            ("double", Layout::Double),
        ] {
            let path = dir.path().join(format!("{name}.nc"));
            write(&path, (100, 130), layout);
            assert!(open(&path).is_none(), "{name} should not take the raw path");
        }
    }

    #[test]
    #[serial_test::serial(netcdf)]
    fn a_window_over_an_unwritten_chunk_is_left_to_netcdf() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("partial.nc");
        let data = write(&path, (200, 130), Layout::PartlyWritten);
        let raw = open(&path).expect("the layout is Ridal's");
        let top = raw.read_window(0, 64, 0, 130).unwrap().unwrap();
        assert_eq!(top, data.slice(ndarray::s![..64, ..]));
        assert!(raw.read_window(60, 70, 0, 10).unwrap().is_none());
    }

    #[test]
    #[serial_test::serial(netcdf)]
    fn chunks_are_cached_up_to_the_budget() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c64.nc");
        write(&path, (300, 520), Layout::Ridal(64));
        let mut raw = open(&path).unwrap();
        raw.cache_capacity = 3;
        raw.read_window(0, 300, 0, 520).unwrap().unwrap();
        let cache = raw.cache.lock().unwrap();
        assert_eq!(cache.len(), 3);
        // The last three chunks read, in the order they were read.
        let n = raw.index.len();
        let keys: Vec<usize> = cache.iter().map(|(k, _)| *k).collect();
        assert_eq!(keys, vec![n - 3, n - 2, n - 1]);
    }

    #[test]
    #[serial_test::serial(netcdf)]
    fn a_radargram_ridal_exported_takes_the_raw_path() {
        // The point of the module: if the writer's layout drifted (another
        // filter, another type), this path would quietly stop being used.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("line.nc");
        crate::gpr::test_fixtures::make_gpr_with_first_break(300, 600)
            .export(&path)
            .unwrap();
        let raw = open(&path).expect("an exported radargram takes the raw path");
        let file = netcdf::open(&path).unwrap();
        let want = file
            .variable("data")
            .unwrap()
            .get::<f32, _>(..)
            .unwrap()
            .into_dimensionality::<ndarray::Ix2>()
            .unwrap();
        let (rows, cols) = (want.shape()[0], want.shape()[1]);
        assert_eq!(raw.read_window(0, rows, 0, cols).unwrap().unwrap(), want);
    }

    #[test]
    fn a_truncated_chunk_is_an_error_not_garbage() {
        let n = 64;
        let values: Vec<u8> = (0..n * 4).map(|i| i as u8).collect();
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(5));
        std::io::Write::write_all(&mut z, &values).unwrap();
        let compressed = z.finish().unwrap();
        assert_eq!(decode(&compressed, n).unwrap().len(), n);
        assert!(decode(&compressed, n + 1).is_err());
        assert!(decode(&compressed[..compressed.len() / 2], n).is_err());
    }
}
