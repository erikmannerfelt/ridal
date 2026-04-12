# Upgrade Notes: pyo3, numpy, ndarray, and netcdf

This document summarizes the attempts to upgrade these interconnected dependencies.

**Latest Update (April 2026)**: CMake 3.31.6 installed from Debian backports - netcdf 0.12 now builds successfully!

## Summary of Attempts

### ✅ Successful Upgrades

| Package | Original | Final | Notes |
|---------|----------|-------|-------|
| **image** | 0.24.6 | 0.25 | Minor version bump |
| **rayon** | 1.7.0 | 1.10 | Minor version bump |
| **clap** | 4.3.2 | 4.5 | Minor version bump |
| **geomorph** | 2.0.2 | 2.0 | Latest in 2.x |
| **utm** | 0.1.6 | 0.1 | Latest in 0.1.x |
| **glob** | 0.3.1 | 0.3 | Latest in 0.3.x |
| **num** | 0.4.0 | 0.4 | Latest in 0.4.x |
| **num-complex** | 0.2.4 | 0.4 | Major version bump |
| **enterpolation** | 0.2 | 0.2 | Kept same (0.3 has breaking API) |

---

## Failed Upgrades

### 1. pyo3 + numpy (0.22 → 0.28)

**Attempted**: Upgrade both to latest (0.28.3 / 0.28.0)

**Result**: ❌ FAILED - Over 100 compilation errors

**Key breaking changes**:
- `PyObject` removed from pyo3 root - now requires `pyo3::ffi::PyObject`
- `into_py()` method removed - replaced with different API
- `from_slice_bound()` renamed to `from_slice()` in numpy
- Many other API changes in pyo3 0.28's migration to free-threaded Python support

**Errors included**:
- Cannot find `PyObject` in scope
- `into_py` method not found
- `from_slice_bound` not found in PyArray
- Multiple "useless conversion" clippy warnings
- Many arguments in functions (clippy)

**Conclusion**: pyo3 0.22 → 0.28 is a massive breaking change requiring significant code rewrites. Deferred to future work.

---

### 2. ndarray + ndarray-stats (0.15.6/0.16 → 0.17)

**Attempted**: Upgrade to 0.17 / 0.7

**Result**: ❌ FAILED for Python builds

**Problem**: 
- numpy 0.22.1 still uses ndarray 0.16
- This creates a dual ndarray version situation (0.16 + 0.17)
- Works for CLI-only builds but fails for Python extension builds

**Error**:
```
expected `&ArrayBase<_, Dim<[usize; 2]>>`, found `&&ArrayBase<OwnedRepr<f32>, ..., f32>`
```

**Partial Success**:
- CLI-only builds work with ndarray 0.17
- All 67 tests pass with ndarray 0.17 + netcdf 0.11
- But Python bindings fail due to numpy's ndarray version

**Conclusion**: Requires numpy upgrade (0.28) which requires pyo3 upgrade (0.28), creating a circular dependency.

---

### 3. netcdf (0.10.5 → 0.11/0.12)

#### Attempt A: netcdf 0.11

**Result**: ✅ PARTIAL SUCCESS

**Changes required**:
```rust
// Old (0.10.5)
.get_into(.., data.view_mut())

// New (0.11+)
.get_into(data.view_mut(), ..)
```

This works for CLI builds. However, Python builds still fail due to the ndarray version conflict (see above).

#### Attempt B: netcdf 0.12

**Result**: ✅ NOW WORKS (April 2026)

**Problem (solved)**: Previously required CMake 3.26+ for static build
- System had CMake 3.25.1 - FAILED
- pip-installed CMake 4.3.1 not used by Cargo's build system - FAILED
- **Solution**: Installed CMake 3.31.6 from Debian bookworm-backports

**Installation steps**:
```bash
# Add Debian backports
echo "deb http://deb.debian.org/debian bookworm-backports main" > /etc/apt/sources.list.d/backports.list
apt-get update
apt-get install -y -t bookworm-backports cmake
```

**Result**: netcdf 0.12 with static build now compiles successfully!

**Current status**: 
- ✅ CLI builds work with netcdf 0.12 + ndarray 0.17
- ✅ All 67 tests pass
- ❌ Python builds still fail (numpy 0.22 uses ndarray 0.16)

**Conclusion**: netcdf 0.12 works for CLI-only. Python extension builds still blocked by numpy/ndarray version mismatch.

---

### 4. enterpolation (0.2 → 0.3)

**Result**: ❌ FAILED

**Breaking changes**:
- `Generator` trait removed from root
- `sample()` method now requires `Signal` trait in scope
- Completely restructured API

**Conclusion**: Kept at 0.2. Would require code changes to upgrade.

---

## Final Working Configuration

```toml
ndarray = "0.16"
ndarray-stats = "0.6"
netcdf = {version = "0.10.5", features = ["ndarray", "static"]}
pyo3 = { version = "0.22", features = ["extension-module"], optional = true}
numpy = { version = "0.22", optional = true }
# ... other deps at latest compatible
```

**Note**: While netcdf 0.12+ now builds, the final configuration keeps 0.10.5 for Python compatibility. 
The CLI-only build can use netcdf 0.12 + ndarray 0.17 successfully.

---

## Path Forward

To upgrade further, the recommended order would be:

1. **First**: Update pyo3 0.22 → 0.28 (most breaking changes)
   - Fix all Python binding code
   - Then numpy can upgrade to 0.28

2. **Second**: numpy 0.22 → 0.28
   - After pyo3 is done
   - This will allow ndarray to upgrade to 0.17

3. **Third**: ndarray 0.16 → 0.17
   - After numpy upgrades
   - Then netcdf can go to 0.11+ (or keep at 0.12 with CMake 3.26+)

4. **Fourth**: netcdf 0.10.5 → 0.11/0.12
   - After ndarray 0.17
   - CMake 3.26+ now available via backports (see above)

5. **Last**: enterpolation 0.2 → 0.3
   - If needed, relatively isolated change

---

## Notes

- The dependency chain is: pyo3 → numpy → ndarray → netcdf
- Each major version bump cascades to the next
- Python extension builds are more constrained than CLI builds
- CLI-only builds can use newer versions than Python builds
- CMake 3.31.6 is now installed system-wide from Debian backports