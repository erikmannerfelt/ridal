//! What a revision's axes were, after its file is gone (#148).
//!
//! ```text
//! revisions/<radargram-id>/<revision-id>.axes
//! ```
//!
//! Re-anchoring needs a revision's *mapping*, not its file. gprinterp SPEC
//! §8.1 carries a coordinate by evaluating it through the axes it was drawn
//! against and inverting the axes it is being read against — so as long as
//! both mappings exist, the picks survive, and 145 MB of amplitudes are not
//! part of the question.
//!
//! That is what buys the right to delete a superseded NetCDF: a few hundred
//! bytes stand in for the only part of it anyone still needs. It also
//! rescues documents written before anchors were emitted, which carry no
//! mapping of their own.
//!
//! # Anchor values, not file axes
//!
//! What is stored is the axis on the **anchor's** scale: travel time from
//! time zero per sample, acquisition time per trace. Not the `twtt` array as
//! the file happens to hold it, which starts at zero whether or not sample
//! zero is time zero (#153). Storing the anchor scale means a later fix to
//! that array changes nothing about what these snapshots mean.
//!
//! # Names, not just numbers
//!
//! The anchor names are stored beside the values. Without them a corrected
//! and an uncorrected revision both present a plausible, linear travel-time
//! axis and re-anchor silently across a difference of metres —
//! erikmannerfelt/gprinterp#9 is the whole argument.
//!
//! # When they are taken
//!
//! On **every** supersession, unconditionally. The tempting optimisation —
//! only snapshot a revision some document references — has a race that
//! cannot be closed: someone with the viewer open, picking against A, has
//! not saved yet, so nothing names A at the moment A is superseded, and
//! their `PUT` arrives later with no anchor and no file to derive one from.
//! The rule assumes a closed world and authoring is not closed.
//!
//! Collecting unreferenced snapshots later is a different and safe
//! question, because by then the window has shut.

#![cfg_attr(
    not(feature = "server"),
    allow(
        dead_code,
        reason = "revisions are superseded through the browser; a CLI-only \
                  build still needs the types to read a project"
    )
)]

use std::io::{Read, Write};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::store::{DocumentStore, Expectation, StoreError};
use crate::identity::RadargramId;

/// Directory holding axis snapshots, relative to the project root.
pub const DIR: &str = "revisions";

/// First bytes of a snapshot file, so a stray file in this directory is
/// recognised as not being one rather than read as garbage.
const MAGIC: &[u8; 8] = b"RIDALAX1";

/// One revision's axes, as the mapping re-anchoring needs.
#[derive(Debug, Clone, PartialEq)]
pub struct AxisSnapshot {
    pub radargram_id: String,
    pub revision_id: String,
    /// Which gprinterp `y` anchor the travel-time values are, or `None` for
    /// a revision that never said. A snapshot without it can be read and
    /// cannot be re-anchored through, which is the correct outcome: see
    /// SPEC §8.3.
    pub y_anchor: Option<String>,
    /// Travel time per sample, on the anchor's scale, nanoseconds.
    pub y_values: Vec<f64>,
    /// Acquisition time per trace, epoch seconds.
    pub x_values: Vec<f64>,
    /// The revision's *other* `y` anchor, as a constant offset from
    /// `y_values`.
    ///
    /// A revision that has located its time zero offers two: `twtt`, and
    /// `recording_time` on the original recording's clock. They are the
    /// same axis read from two origins, so they differ by exactly the
    /// position of time zero — one number, not a second array.
    ///
    /// Keeping only the preferred one made a corrected revision's
    /// snapshot unable to relate to an *uncorrected* revision, which
    /// offers only `recording_time`: no shared anchor, even though both
    /// files have the recording clock and the mapping is exact. `None`
    /// for a revision with one anchor, and for every snapshot written
    /// before this field existed.
    pub y_alternate: Option<AlternateAnchor>,
}

/// A second `y` anchor for the same samples, a fixed distance away.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AlternateAnchor {
    pub name: String,
    /// Added to each of `y_values` to get this anchor's value.
    pub offset: f64,
}

impl AxisSnapshot {
    pub fn n_samples(&self) -> usize {
        self.y_values.len()
    }

    pub fn n_traces(&self) -> usize {
        self.x_values.len()
    }

    /// A fingerprint of the axes themselves.
    ///
    /// This is the question `RevisionId` cannot answer. That is
    /// `hash(radargram_id + processing_datetime)` and says nothing about
    /// contents, so a different file carrying the same datetime — which
    /// Ridal cannot produce but another tool can — gets the *same* revision
    /// id, and every staleness check believes nothing changed. Comparing
    /// this against what the ledger recorded is how that is caught.
    pub fn checksum(&self) -> String {
        let mut hasher = blake3::Hasher::new();
        hasher.update(self.y_anchor.as_deref().unwrap_or("").as_bytes());
        // `y_alternate` is deliberately *not* hashed. It is derived from
        // the same declarations as `y_values` -- the same axis read from a
        // second origin -- so it carries no information the values and the
        // anchor name do not already fix. Hashing it would only mean that
        // adding the field changed every checksum, and every ledger
        // written before it would then read as a revision that had
        // changed underneath us.
        hasher.update(&(self.y_values.len() as u64).to_le_bytes());
        hasher.update(&(self.x_values.len() as u64).to_le_bytes());
        for value in self.y_values.iter().chain(self.x_values.iter()) {
            hasher.update(&value.to_le_bytes());
        }
        hasher.finalize().to_hex().to_string()
    }
}

/// The header, in plain JSON ahead of the compressed values.
///
/// Uncompressed on purpose. This is a binary file living in a directory
/// people will open when something has gone wrong, and `head -c 200` naming
/// the radargram, the revision and the anchor is worth more than the
/// hundred bytes it costs.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Header {
    radargram_id: String,
    revision_id: String,
    y_anchor: Option<String>,
    n_samples: usize,
    n_traces: usize,
    /// Absent in every snapshot written before it existed, which is what
    /// `default` is carrying: those are readable unchanged and simply
    /// offer one anchor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    y_alternate: Option<AlternateAnchor>,
}

#[derive(Debug)]
pub enum SnapshotError {
    Store(StoreError),
    Malformed {
        path: PathBuf,
        message: String,
    },
    /// A snapshot already exists for this revision id and says something
    /// else. See [`put`] for why this is refused rather than resolved.
    Collision {
        radargram_id: String,
        revision_id: String,
    },
}

impl std::fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SnapshotError::Store(e) => write!(f, "{e}"),
            SnapshotError::Malformed { path, message } => {
                write!(
                    f,
                    "{} is not a usable axis snapshot: {message}",
                    path.display()
                )
            }
            SnapshotError::Collision {
                radargram_id,
                revision_id,
            } => {
                write!(
                    f,
                    "'{radargram_id}' already has axes kept for revision {revision_id}, \
                     and they describe a different mapping. Two files are sharing one \
                     revision id, which means they share a processing datetime: \
                     reprocess one of them so it gets its own."
                )
            }
        }
    }
}

impl std::error::Error for SnapshotError {}

impl From<StoreError> for SnapshotError {
    fn from(e: StoreError) -> Self {
        SnapshotError::Store(e)
    }
}

fn path_of(radargram: &RadargramId, revision: &str) -> PathBuf {
    PathBuf::from(DIR)
        .join(radargram.as_str())
        .join(format!("{revision}.axes"))
}

/// Delta-encode, so deflate has something to work with.
///
/// Both axes are monotone and near-evenly spaced, so the differences are
/// nearly constant and the compressor collapses them. Measured on a real
/// radargram (1988 samples, 2529 traces): 36 kB of `f64` becomes 12.9 kB
/// compressed raw, and **691 bytes** delta-encoded first. Lossless either
/// way — quantising to a tenth of a sample interval was no smaller and
/// threw away precision for nothing.
fn encode(values: &[f64]) -> Vec<u8> {
    let mut out = Vec::with_capacity(values.len() * 8);
    let mut previous = 0.0;
    for value in values {
        out.extend_from_slice(&(value - previous).to_le_bytes());
        previous = *value;
    }
    out
}

fn decode(bytes: &[u8], count: usize) -> Option<Vec<f64>> {
    // Checked, because `count` comes out of a header on disk. A crafted
    // one large enough to wrap `usize` would otherwise make this length
    // check pass on a short payload, and `with_capacity` ask for the
    // wrapped size -- a panic or a failed allocation where the answer is
    // "this file is malformed".
    if count.checked_mul(8) != Some(bytes.len()) {
        return None;
    }
    let mut out = Vec::with_capacity(count);
    let mut running = 0.0;
    // `as_chunks` rather than `chunks_exact(8)`: the width is a constant,
    // so the compiler can have it, and each chunk arrives as `[u8; 8]`
    // without a fallible conversion that cannot actually fail.
    let (chunks, remainder) = bytes.as_chunks::<8>();
    if !remainder.is_empty() {
        return None;
    }
    for chunk in chunks {
        running += f64::from_le_bytes(*chunk);
        out.push(running);
    }
    Some(out)
}

/// Serialize a snapshot: magic, header length, header, deflated values.
fn to_bytes(snapshot: &AxisSnapshot) -> Result<Vec<u8>, SnapshotError> {
    let header = Header {
        radargram_id: snapshot.radargram_id.clone(),
        revision_id: snapshot.revision_id.clone(),
        y_anchor: snapshot.y_anchor.clone(),
        n_samples: snapshot.n_samples(),
        n_traces: snapshot.n_traces(),
        y_alternate: snapshot.y_alternate.clone(),
    };
    let header = serde_json::to_vec(&header).map_err(|e| SnapshotError::Malformed {
        path: PathBuf::from(DIR),
        message: e.to_string(),
    })?;

    let mut payload = encode(&snapshot.y_values);
    payload.extend_from_slice(&encode(&snapshot.x_values));
    let mut encoder = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::new(6));
    encoder
        .write_all(&payload)
        .and_then(|_| encoder.finish())
        .map_err(|e| SnapshotError::Malformed {
            path: PathBuf::from(DIR),
            message: format!("could not compress the axes: {e}"),
        })
        .map(|compressed| {
            let mut out = Vec::with_capacity(16 + header.len() + compressed.len());
            out.extend_from_slice(MAGIC);
            out.extend_from_slice(&(header.len() as u32).to_le_bytes());
            out.extend_from_slice(&header);
            out.extend_from_slice(&compressed);
            out
        })
}

fn from_bytes(bytes: &[u8], path: PathBuf) -> Result<AxisSnapshot, SnapshotError> {
    let bad = |message: &str| SnapshotError::Malformed {
        path: path.clone(),
        message: message.to_string(),
    };
    if bytes.len() < 12 || &bytes[..8] != MAGIC {
        return Err(bad("not a Ridal axis snapshot"));
    }
    let header_len = u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize;
    let header_end = 12 + header_len;
    if bytes.len() < header_end {
        return Err(bad("the header is truncated"));
    }
    let header: Header = serde_json::from_slice(&bytes[12..header_end])
        .map_err(|e| bad(&format!("the header is not readable: {e}")))?;

    let mut payload = Vec::new();
    flate2::read::DeflateDecoder::new(&bytes[header_end..])
        .read_to_end(&mut payload)
        .map_err(|e| bad(&format!("the axes could not be decompressed: {e}")))?;

    // Checked all the way, for the same reason: every one of these
    // lengths is a number this process read from a file rather than one
    // it computed.
    let split = header
        .n_samples
        .checked_mul(8)
        .ok_or_else(|| bad("bad y axis"))?;
    let x_bytes = header
        .n_traces
        .checked_mul(8)
        .ok_or_else(|| bad("bad x axis"))?;
    let total = split
        .checked_add(x_bytes)
        .ok_or_else(|| bad("bad axis lengths"))?;
    if payload.len() != total {
        return Err(bad("the axes are not the length the header claims"));
    }
    let y_values = decode(&payload[..split], header.n_samples).ok_or_else(|| bad("bad y axis"))?;
    let x_values = decode(&payload[split..], header.n_traces).ok_or_else(|| bad("bad x axis"))?;

    Ok(AxisSnapshot {
        radargram_id: header.radargram_id,
        revision_id: header.revision_id,
        y_anchor: header.y_anchor,
        y_values,
        x_values,
        y_alternate: header.y_alternate,
    })
}

/// Keep a revision's axes.
///
/// Idempotent: writing the same revision twice is not an error, because a
/// supersession that is retried after a partial failure must be able to
/// finish. The content is a function of the revision, so a second write of
/// the same revision writes the same bytes.
pub fn put(
    store: &DocumentStore,
    radargram: &RadargramId,
    snapshot: &AxisSnapshot,
) -> Result<(), SnapshotError> {
    let bytes = to_bytes(snapshot)?;
    let relative = path_of(radargram, &snapshot.revision_id);

    // A revision id is `hash(radargram_id + processing_datetime)` and says
    // nothing about contents, so two different files carrying the same
    // datetime -- which Ridal cannot produce but another tool can -- land
    // here under one name. Overwriting would re-anchor documents drawn on
    // the first through the second's axes, silently, which is the precise
    // failure the ledger's checksum exists to catch: catching it there and
    // permitting it here would be answering the question and ignoring the
    // answer.
    //
    // Compared as a *mapping*, not as bytes.
    //
    // Byte equality was the first thing here and it was wrong: it
    // conflates "these two files describe different axes" with "the way
    // we serialize axes has changed since this was written". Adding a
    // field to the stored header made every existing snapshot stop
    // matching the bytes this build produces for the very same file, and
    // the code then accused the operator of having two files that share a
    // processing datetime -- which was not true, and which they could do
    // nothing about.
    //
    // So a re-snapshot of an unchanged revision is recognised by what it
    // says, and the collision that matters -- genuinely different axes
    // under one revision id -- still refuses.
    if let Some((existing, version)) = store.read_bytes(&relative)? {
        if existing == bytes {
            return Ok(());
        }
        let stored = from_bytes(&existing, store.root().join(&relative))?;
        if !describes_same_mapping(&stored, snapshot) {
            return Err(SnapshotError::Collision {
                radargram_id: radargram.to_string(),
                revision_id: snapshot.revision_id.clone(),
            });
        }
        // The same mapping, written by an older build. Upgraded in place
        // when this one has something to add -- the alternate anchor is
        // what lets a corrected revision relate to an uncorrected one, and
        // a snapshot that predates it would otherwise never gain it.
        if snapshot.y_alternate.is_some() && stored.y_alternate.is_none() {
            store.write_bytes(&relative, &bytes, &Expectation::Version(version))?;
        }
        return Ok(());
    }

    // `Absent` rather than `Any`, so the check above is not merely advisory:
    // a second writer between the read and this write is refused instead of
    // overwritten.
    store.write_bytes(&relative, &bytes, &Expectation::Absent)?;
    Ok(())
}

/// Whether two snapshots describe the same axes.
///
/// Not derived `PartialEq`: the question is whether the *mapping* is the
/// same, so a snapshot that has since gained a second anchor still matches
/// the one that only had the first. The values are compared with a
/// tolerance because they are round-tripped through a delta encoding,
/// where a sum of differences need not reproduce the original bit for bit.
fn describes_same_mapping(a: &AxisSnapshot, b: &AxisSnapshot) -> bool {
    fn close(a: &[f64], b: &[f64]) -> bool {
        a.len() == b.len()
            && a.iter().zip(b).all(|(x, y)| {
                let scale = x.abs().max(y.abs()).max(1.0);
                (x - y).abs() <= f64::EPSILON * scale * 16.0
            })
    }
    a.y_anchor == b.y_anchor && close(&a.y_values, &b.y_values) && close(&a.x_values, &b.x_values)
}

/// Read a revision's axes, or `None` if none were kept.
pub fn get(
    store: &DocumentStore,
    radargram: &RadargramId,
    revision: &str,
) -> Result<Option<AxisSnapshot>, SnapshotError> {
    let relative = path_of(radargram, revision);
    let Some((bytes, _)) = store.read_bytes(&relative)? else {
        return Ok(None);
    };
    Ok(Some(from_bytes(&bytes, store.root().join(&relative))?))
}

/// Every revision of `radargram` that has a snapshot.
#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "the caller arrives with re-anchoring on read (#148): \
                  deciding what to offer for an old document means knowing \
                  which mappings are available"
    )
)]
pub fn list(store: &DocumentStore, radargram: &RadargramId) -> Result<Vec<String>, SnapshotError> {
    Ok(store.list_stems(&PathBuf::from(DIR).join(radargram.as_str()), ".axes")?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, DocumentStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = DocumentStore::new(dir.path().to_path_buf());
        (dir, store)
    }

    fn snapshot(revision: &str) -> AxisSnapshot {
        AxisSnapshot {
            radargram_id: "line-01".to_string(),
            revision_id: revision.to_string(),
            y_anchor: Some("twtt".to_string()),
            y_values: (0..1988).map(|i| i as f64 * 1.2407).collect(),
            x_values: (0..2529)
                .map(|i| 1_648_557_660.0 + i as f64 / 3.0)
                .collect(),
            y_alternate: None,
        }
    }

    #[test]
    fn a_header_claiming_an_impossible_size_is_malformed_rather_than_a_panic() {
        // The counts come out of a file, so they are input. A crafted one
        // large enough to wrap `usize` made the length check pass on a
        // short payload and `with_capacity` ask for the wrapped size --
        // an abort where the answer is "this file is malformed".
        let (_dir, store) = store();
        let id = RadargramId::new("line-01").unwrap();
        put(&store, &id, &snapshot("rev-a")).unwrap();

        let relative = path_of(&id, "rev-a");
        let (bytes, _) = store.read_bytes(&relative).unwrap().unwrap();

        // Rewrite the header with counts whose byte lengths overflow.
        let header_len = u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize;
        let mut header: serde_json::Value =
            serde_json::from_slice(&bytes[12..12 + header_len]).unwrap();
        header["n_samples"] = serde_json::json!(usize::MAX / 4);
        header["n_traces"] = serde_json::json!(usize::MAX / 4);
        let header = serde_json::to_vec(&header).unwrap();

        let mut crafted = Vec::new();
        crafted.extend_from_slice(MAGIC);
        crafted.extend_from_slice(&(header.len() as u32).to_le_bytes());
        crafted.extend_from_slice(&header);
        crafted.extend_from_slice(&bytes[12 + header_len..]);
        store
            .write_bytes(&relative, &crafted, &Expectation::Any)
            .unwrap();

        let read = get(&store, &id, "rev-a");
        assert!(
            matches!(read, Err(SnapshotError::Malformed { .. })),
            "{read:?}"
        );
    }

    #[test]
    fn a_second_file_claiming_one_revision_id_is_refused_rather_than_overwriting() {
        // `RevisionId` is hash(radargram_id + processing_datetime) and says
        // nothing about contents, so two different files carrying the same
        // datetime land under one name. Overwriting would re-anchor
        // documents drawn on the first through the second's axes -- the
        // precise failure the ledger's checksum exists to catch.
        let (_dir, store) = store();
        let id = RadargramId::new("line-01").unwrap();

        put(&store, &id, &snapshot("rev-a")).unwrap();

        let mut different = snapshot("rev-a");
        different.y_values = different.y_values.iter().map(|v| v + 3.0).collect();
        let refused = put(&store, &id, &different).unwrap_err();
        assert!(
            matches!(refused, SnapshotError::Collision { .. }),
            "{refused}"
        );

        // And the first one is untouched, which is the point.
        let kept = get(&store, &id, "rev-a").unwrap().unwrap();
        assert_eq!(kept, snapshot("rev-a"));
    }

    #[test]
    fn a_snapshot_written_by_an_older_build_is_not_a_collision() {
        // Reported: switching a radargram back and forth between two
        // revisions eventually refused with "two files are sharing one
        // revision id", naming a datetime clash the operator had not
        // created and could not fix.
        //
        // The check compared serialized bytes, so adding a field to the
        // stored header made every snapshot written before it stop
        // matching the bytes this build produces for the *same file*. The
        // question is whether the mapping differs, not whether the
        // encoding has.
        let (_dir, store) = store();
        let id = RadargramId::new("line-01").unwrap();

        // What an older build wrote: the same axes, no alternate anchor.
        let old = snapshot("rev-a");
        put(&store, &id, &old).unwrap();

        // What this build produces from the same radargram.
        let mut now = snapshot("rev-a");
        now.y_alternate = Some(AlternateAnchor {
            name: "recording_time".to_string(),
            offset: 50.757_175,
        });
        put(&store, &id, &now).expect("the same mapping, said more fully");

        // And it is upgraded in place, so the alternate is not lost --
        // that anchor is what lets a corrected revision relate to an
        // uncorrected one.
        let stored = get(&store, &id, "rev-a").unwrap().unwrap();
        assert_eq!(
            stored.y_alternate.as_ref().map(|a| a.name.as_str()),
            Some("recording_time")
        );

        // Genuinely different axes under one revision id still refuse.
        let mut different = now.clone();
        different.y_values = different.y_values.iter().map(|v| v + 3.0).collect();
        assert!(matches!(
            put(&store, &id, &different),
            Err(SnapshotError::Collision { .. })
        ));
    }

    #[test]
    fn the_checksum_does_not_move_when_a_snapshot_gains_an_alternate() {
        // The ledger records a checksum when a revision is superseded, and
        // `axes_for_revision_checked` compares against it. If adding the
        // alternate changed the checksum, every project with a ledger
        // written before it would report that its radargrams had been
        // rewritten behind its back.
        //
        // It does not, because the alternate is derived from the same
        // declarations as the values: it is the same axis read from a
        // second origin, and fixes nothing the values and anchor name do
        // not already fix.
        let plain = snapshot("rev-a");
        let mut richer = snapshot("rev-a");
        richer.y_alternate = Some(AlternateAnchor {
            name: "recording_time".to_string(),
            offset: 50.757_175,
        });
        assert_eq!(plain.checksum(), richer.checksum());

        // What the checksum *is* for still moves it.
        let mut changed = plain.clone();
        changed.y_values = changed.y_values.iter().map(|v| v + 1.0).collect();
        assert_ne!(plain.checksum(), changed.checksum());
    }

    #[test]
    fn writing_the_same_snapshot_twice_is_not_a_collision() {
        // A re-run, not two files. `to_bytes` is deterministic, so the same
        // mapping encodes the same way, and refusing it would turn an
        // idempotent operation into an error.
        let (_dir, store) = store();
        let id = RadargramId::new("line-01").unwrap();
        put(&store, &id, &snapshot("rev-a")).unwrap();
        put(&store, &id, &snapshot("rev-a")).expect("the same mapping again");
    }

    #[test]
    fn a_snapshot_round_trips_exactly() {
        // Lossless is the point: a re-anchored coordinate is approximate
        // enough already without the mapping it was carried through having
        // been rounded on the way into storage.
        let (_dir, store) = store();
        let original = snapshot("rev-a");
        put(&store, &RadargramId::new("line-01").unwrap(), &original).unwrap();

        let read = get(&store, &RadargramId::new("line-01").unwrap(), "rev-a")
            .unwrap()
            .unwrap();
        assert_eq!(read, original);
        assert_eq!(read.checksum(), original.checksum());
    }

    #[test]
    fn the_axes_compress_to_a_fraction_of_their_size() {
        // What buys the right to delete a 145 MB NetCDF. If this ever stops
        // holding, keeping a snapshot per supersession stops being free and
        // the decision deserves revisiting.
        let (dir, store) = store();
        let original = snapshot("rev-a");
        put(&store, &RadargramId::new("line-01").unwrap(), &original).unwrap();

        let raw = (original.n_samples() + original.n_traces()) * 8;
        let stored = std::fs::metadata(dir.path().join("revisions/line-01/rev-a.axes"))
            .unwrap()
            .len() as usize;
        assert!(
            stored * 10 < raw,
            "expected well under a tenth of {raw} raw bytes, got {stored}"
        );
    }

    #[test]
    fn the_header_is_readable_without_decompressing_anything() {
        // A binary file in a directory people open when something has gone
        // wrong. `head -c 200` naming the radargram and the anchor is worth
        // the hundred bytes.
        let (dir, store) = store();
        put(
            &store,
            &RadargramId::new("line-01").unwrap(),
            &snapshot("rev-a"),
        )
        .unwrap();

        let bytes = std::fs::read(dir.path().join("revisions/line-01/rev-a.axes")).unwrap();
        let head = String::from_utf8_lossy(&bytes[..200.min(bytes.len())]).to_string();
        assert!(head.starts_with("RIDALAX1"), "{head}");
        assert!(head.contains("line-01"), "{head}");
        assert!(head.contains("twtt"), "{head}");
    }

    #[test]
    fn the_checksum_notices_a_changed_axis_that_the_revision_id_would_not() {
        // The datetime collision: `RevisionId` is
        // hash(radargram_id + processing_datetime) and says nothing about
        // contents, so a different file carrying the same datetime gets the
        // same id and every staleness check believes nothing changed.
        let mut a = snapshot("rev-a");
        let mut b = a.clone();
        b.y_values[500] += 0.001;
        assert_ne!(
            a.checksum(),
            b.checksum(),
            "a moved sample is a changed axis"
        );

        // And the anchor name is part of it, which is what keeps a
        // corrected and an uncorrected revision apart even when their
        // numbers coincide.
        a.y_anchor = Some("twtt_normal_incidence".to_string());
        let mut same_numbers = a.clone();
        same_numbers.y_anchor = Some("twtt".to_string());
        assert_ne!(a.checksum(), same_numbers.checksum());
    }

    #[test]
    fn a_file_that_is_not_a_snapshot_is_refused_rather_than_read_as_garbage() {
        let (_dir, store) = store();
        let id = RadargramId::new("line-01").unwrap();
        store
            .write(&path_of(&id, "rev-a"), "just some text", &Expectation::Any)
            .unwrap();
        assert!(matches!(
            get(&store, &id, "rev-a"),
            Err(SnapshotError::Malformed { .. })
        ));
    }

    #[test]
    fn a_truncated_snapshot_is_refused() {
        // Half a mapping is worse than none: it would re-anchor the
        // coordinates it happened to cover and silently drop the rest.
        let (dir, store) = store();
        let id = RadargramId::new("line-01").unwrap();
        put(&store, &id, &snapshot("rev-a")).unwrap();

        let path = dir.path().join("revisions/line-01/rev-a.axes");
        let bytes = std::fs::read(&path).unwrap();
        std::fs::write(&path, &bytes[..bytes.len() / 2]).unwrap();

        assert!(matches!(
            get(&store, &id, "rev-a"),
            Err(SnapshotError::Malformed { .. })
        ));
    }

    #[test]
    fn snapshots_are_listed_per_radargram() {
        let (_dir, store) = store();
        let id = RadargramId::new("line-01").unwrap();
        put(&store, &id, &snapshot("rev-a")).unwrap();
        put(&store, &id, &snapshot("rev-b")).unwrap();
        // Writing the same revision twice is not an error: a supersession
        // retried after a partial failure has to be able to finish.
        put(&store, &id, &snapshot("rev-a")).unwrap();

        let mut listed = list(&store, &id).unwrap();
        listed.sort();
        assert_eq!(listed, vec!["rev-a", "rev-b"]);
        assert!(list(&store, &RadargramId::new("untouched").unwrap())
            .unwrap()
            .is_empty());
    }
}

/// What a radargram id has been, in order (#148).
///
/// ```text
/// revisions.json
/// ```
///
/// Kept apart from the snapshots because it answers a different question.
/// A snapshot is one revision's mapping; this is the *history* — which
/// revision superseded which, when, and whether their axes were really
/// different. Re-anchoring needs a pair of snapshots; deciding what to tell
/// somebody about a document needs this.
pub mod ledger {
    use super::*;
    use std::collections::BTreeMap;

    /// The document's path, relative to the project root.
    pub const FILE: &str = "revisions.json";

    /// One revision a radargram has been through.
    #[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
    pub struct RevisionRecord {
        pub revision_id: String,
        /// A fingerprint of the axes, which is what `revision_id` cannot
        /// give: that is `hash(radargram_id + processing_datetime)` and
        /// says nothing about contents.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub axis_checksum: Option<String>,
        /// When it stopped being current, RFC 3339. `None` for the one
        /// that still is.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub superseded_at: Option<String>,
        /// What replaced it, or `None` for a removal — which is a
        /// supersession with nothing on the other side of it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub superseded_by: Option<String>,
    }

    #[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
    pub struct Ledger {
        /// Oldest first, per radargram.
        #[serde(default)]
        pub radargrams: BTreeMap<String, Vec<RevisionRecord>>,
    }

    impl Ledger {
        /// The revision currently behind `radargram`, if the ledger knows.
        pub fn current(&self, radargram: &str) -> Option<&RevisionRecord> {
            self.radargrams
                .get(radargram)?
                .iter()
                .rev()
                .find(|record| record.superseded_at.is_none())
        }

        /// Whether this radargram has been superseded at least once, which
        /// is what makes a document's `source.revision_id` worth checking.
        #[cfg_attr(
            not(test),
            allow(
                dead_code,
                reason = "the caller arrives with the load banner (#148): a \
                          radargram with no history cannot have a stale \
                          document against it"
            )
        )]
        pub fn has_history(&self, radargram: &str) -> bool {
            self.radargrams
                .get(radargram)
                .is_some_and(|records| records.len() > 1)
        }
    }

    fn path_of() -> PathBuf {
        PathBuf::from(FILE)
    }

    pub fn read(store: &DocumentStore) -> Result<(Ledger, Expectation), SnapshotError> {
        let relative = path_of();
        let Some(stored) = store.read(&relative)? else {
            return Ok((Ledger::default(), Expectation::Absent));
        };
        let parsed: Ledger =
            serde_json::from_str(&stored.text).map_err(|e| SnapshotError::Malformed {
                path: store.root().join(&relative),
                message: e.to_string(),
            })?;
        Ok((parsed, Expectation::Version(stored.version)))
    }

    /// Read, modify, write, conditional on what was read.
    pub fn update<T>(
        store: &DocumentStore,
        change: impl Fn(&mut Ledger) -> T,
    ) -> Result<T, SnapshotError> {
        let mut attempts = 0;
        loop {
            attempts += 1;
            let (mut ledger, expectation) = read(store)?;
            let outcome = change(&mut ledger);
            let mut text =
                serde_json::to_string_pretty(&ledger).map_err(|e| SnapshotError::Malformed {
                    path: store.root().join(path_of()),
                    message: e.to_string(),
                })?;
            text.push('\n');
            match store.write(&path_of(), &text, &expectation) {
                Ok(_) => return Ok(outcome),
                Err(StoreError::Conflict { .. }) if attempts < 3 => continue,
                Err(e) => return Err(e.into()),
            }
        }
    }

    /// Record that a revision is current, if the ledger does not know it.
    ///
    /// Called at discovery, so a radargram that has never been superseded
    /// still has its axis checksum on file. Without that, the *first*
    /// supersession has nothing to compare against and the datetime
    /// collision is undetectable exactly when it matters most — the moment
    /// something replaces a file.
    pub fn note_current(
        ledger: &mut Ledger,
        radargram: &str,
        revision: &str,
        checksum: Option<String>,
    ) {
        let records = ledger.radargrams.entry(radargram.to_string()).or_default();
        if let Some(existing) = records.iter_mut().find(|r| r.revision_id == revision) {
            // Filled in rather than overwritten: a checksum already there
            // is what a later comparison is *against*, and replacing it
            // with a fresh reading would quietly answer "unchanged" to the
            // question it exists to ask.
            if existing.axis_checksum.is_none() {
                existing.axis_checksum = checksum;
            }
            return;
        }
        records.push(RevisionRecord {
            revision_id: revision.to_string(),
            axis_checksum: checksum,
            superseded_at: None,
            superseded_by: None,
        });
    }

    /// Record that a revision has been superseded.
    ///
    /// `by` is `None` for a removal, which is a supersession with nothing
    /// on the other side of it — the radargram id stops having a current
    /// revision, and any document drawn on the old one keeps its meaning
    /// through the snapshot rather than through a file.
    pub fn supersede(
        ledger: &mut Ledger,
        radargram: &str,
        revision: &str,
        by: Option<&str>,
        at: &str,
    ) {
        let records = ledger.radargrams.entry(radargram.to_string()).or_default();
        if let Some(existing) = records.iter_mut().find(|r| r.revision_id == revision) {
            existing.superseded_at = Some(at.to_string());
            existing.superseded_by = by.map(str::to_string);
        } else {
            records.push(RevisionRecord {
                revision_id: revision.to_string(),
                axis_checksum: None,
                superseded_at: Some(at.to_string()),
                superseded_by: by.map(str::to_string),
            });
        }
        if let Some(by) = by {
            note_current(ledger, radargram, by, None);
        }
    }

    /// Record that `revision` is the id's current one, undoing a
    /// supersession if it had been marked as one.
    ///
    /// [`note_current`] alone is not enough here. A radargram can come back:
    /// an ignore is lifted, or a file is removed and the same revision
    /// re-uploaded. `note_current` leaves an existing record untouched, so
    /// the radargram would go on being served while its own history says it
    /// was superseded — a report that contradicts what the catalog is
    /// showing, which is worse than no report.
    ///
    /// Only the named revision is cleared. Genuinely older revisions of the
    /// same id stay superseded, because they are.
    pub fn note_current_again(
        ledger: &mut Ledger,
        radargram: &str,
        revision: &str,
        checksum: Option<String>,
    ) {
        note_current(ledger, radargram, revision, checksum);
        if let Some(records) = ledger.radargrams.get_mut(radargram) {
            if let Some(existing) = records.iter_mut().find(|r| r.revision_id == revision) {
                existing.superseded_at = None;
                existing.superseded_by = None;
            }
        }
    }
}

#[cfg(test)]
mod ledger_tests {
    use super::ledger::*;
    use super::*;

    fn store() -> (tempfile::TempDir, DocumentStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = DocumentStore::new(dir.path().to_path_buf());
        (dir, store)
    }

    #[test]
    fn a_revision_that_becomes_current_again_stops_being_superseded() {
        // A radargram can come back: an ignore is lifted, or a removed id
        // is re-uploaded. `note_current` leaves an existing record alone,
        // so without this the catalog would serve a radargram whose own
        // history says it was superseded.
        let (_dir, store) = store();
        update(&store, |l| {
            note_current(l, "line-01", "rev-a", Some("abc123".into()));
            supersede(l, "line-01", "rev-a", None, "2026-09-13T12:00:00Z");
        })
        .unwrap();

        let (ledger, _) = read(&store).unwrap();
        assert!(ledger.current("line-01").is_none(), "removed");

        update(&store, |l| {
            note_current_again(l, "line-01", "rev-a", None);
        })
        .unwrap();

        let (ledger, _) = read(&store).unwrap();
        let current = ledger.current("line-01").expect("current again");
        assert_eq!(current.revision_id, "rev-a");
        assert!(current.superseded_at.is_none());
        assert!(current.superseded_by.is_none());
        assert_eq!(
            current.axis_checksum.as_deref(),
            Some("abc123"),
            "and the baseline it came back with is still the one to compare against"
        );
    }

    #[test]
    fn coming_back_does_not_revive_the_revisions_that_really_were_superseded() {
        // Only the named revision. An older one is superseded because it
        // is, and un-superseding it would say two revisions are current.
        let (_dir, store) = store();
        update(&store, |l| {
            note_current(l, "line-01", "rev-a", None);
            supersede(l, "line-01", "rev-a", Some("rev-b"), "2026-09-13T12:00:00Z");
            supersede(l, "line-01", "rev-b", None, "2026-09-14T12:00:00Z");
        })
        .unwrap();

        update(&store, |l| {
            note_current_again(l, "line-01", "rev-b", None);
        })
        .unwrap();

        let (ledger, _) = read(&store).unwrap();
        assert_eq!(ledger.current("line-01").unwrap().revision_id, "rev-b");
        let records = &ledger.radargrams["line-01"];
        let older = records.iter().find(|r| r.revision_id == "rev-a").unwrap();
        assert_eq!(
            older.superseded_by.as_deref(),
            Some("rev-b"),
            "the one that really was superseded stayed that way"
        );
    }

    #[test]
    fn a_radargram_with_no_history_still_has_its_checksum_recorded() {
        // Without this the first supersession has nothing to compare
        // against, and the datetime collision is undetectable exactly when
        // it matters most.
        let (_dir, store) = store();
        update(&store, |ledger| {
            note_current(ledger, "line-01", "rev-a", Some("abc123".into()));
        })
        .unwrap();

        let (ledger, _) = read(&store).unwrap();
        let current = ledger.current("line-01").unwrap();
        assert_eq!(current.revision_id, "rev-a");
        assert_eq!(current.axis_checksum.as_deref(), Some("abc123"));
        assert!(
            !ledger.has_history("line-01"),
            "one revision is not a history"
        );
    }

    #[test]
    fn a_checksum_already_on_file_is_not_overwritten_by_a_fresh_reading() {
        // It is what a later comparison is *against*. Replacing it would
        // quietly answer "unchanged" to the question it exists to ask.
        let (_dir, store) = store();
        update(&store, |l| {
            note_current(l, "line-01", "rev-a", Some("original".into()))
        })
        .unwrap();
        update(&store, |l| {
            note_current(l, "line-01", "rev-a", Some("different".into()))
        })
        .unwrap();

        let (ledger, _) = read(&store).unwrap();
        assert_eq!(
            ledger.current("line-01").unwrap().axis_checksum.as_deref(),
            Some("original")
        );
    }

    #[test]
    fn superseding_records_both_sides_and_moves_current_forward() {
        let (_dir, store) = store();
        update(&store, |l| {
            note_current(l, "line-01", "rev-a", Some("aaa".into()))
        })
        .unwrap();
        update(&store, |l| {
            supersede(l, "line-01", "rev-a", Some("rev-b"), "2026-09-13T12:00:00Z")
        })
        .unwrap();

        let (ledger, _) = read(&store).unwrap();
        let records = &ledger.radargrams["line-01"];
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].superseded_by.as_deref(), Some("rev-b"));
        assert_eq!(
            records[0].superseded_at.as_deref(),
            Some("2026-09-13T12:00:00Z")
        );
        assert_eq!(ledger.current("line-01").unwrap().revision_id, "rev-b");
        assert!(ledger.has_history("line-01"));
    }

    #[test]
    fn a_removal_is_a_supersession_with_nothing_on_the_other_side() {
        // The radargram id stops having a current revision. Any document
        // drawn on the old one keeps its meaning through the snapshot
        // rather than through a file that is no longer there.
        let (_dir, store) = store();
        update(&store, |l| {
            note_current(l, "line-01", "rev-a", Some("aaa".into()))
        })
        .unwrap();
        update(&store, |l| {
            supersede(l, "line-01", "rev-a", None, "2026-09-13T12:00:00Z")
        })
        .unwrap();

        let (ledger, _) = read(&store).unwrap();
        assert!(
            ledger.current("line-01").is_none(),
            "nothing is current now"
        );
        assert_eq!(ledger.radargrams["line-01"][0].superseded_by, None);
        // And the record of what it was is still there to re-anchor from.
        assert_eq!(
            ledger.radargrams["line-01"][0].axis_checksum.as_deref(),
            Some("aaa")
        );
    }
}
