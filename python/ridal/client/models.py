"""What the server answers, as plain records.

Each class mirrors one schema in the server's OpenAPI description
(``docs/reference/openapi.json``, served at ``/api/v1/openapi.json``), field
for field; a test holds them to it. Fields the server sends as ``null`` are
``None`` here.
"""

import dataclasses
from collections.abc import Callable, Mapping
from dataclasses import dataclass
from typing import Any, Literal, TypeAlias, TypeVar

import numpy as np

from ridal.client import errors

Role: TypeAlias = Literal["viewer", "picker", "operator", "admin"]
DownloadScope: TypeAlias = Literal["none", "results", "picks", "derived", "all"]
Severity: TypeAlias = Literal["current", "carried", "approximate", "partial", "refused"]

_T = TypeVar("_T")


def _parse(kind: str, data: Any, build: Callable[[Mapping[str, Any]], _T]) -> _T:
    """Build a record, turning a malformed answer into a :class:`ProtocolError`."""
    if not isinstance(data, Mapping):
        raise errors.ProtocolError(
            f"expected a {kind} object, got {type(data).__name__}"
        )
    try:
        return build(data)
    except (KeyError, TypeError, ValueError) as error:
        raise errors.ProtocolError(
            f"the server's {kind} is not understood: {error!r}"
        ) from error


@dataclass(frozen=True)
class Health:
    """``GET /api/v1/health``."""

    status: str
    version: str

    @classmethod
    def from_json(cls, data: Any) -> "Health":
        return _parse(
            "health", data, lambda d: cls(status=d["status"], version=d["version"])
        )


@dataclass(frozen=True)
class Grant:
    """What an API token may do in one project, at most."""

    project: str
    role: Role
    download: DownloadScope

    @classmethod
    def from_json(cls, data: Any) -> "Grant":
        return _parse(
            "grant",
            data,
            lambda d: cls(project=d["project"], role=d["role"], download=d["download"]),
        )


@dataclass(frozen=True)
class TokenSummary:
    """The API token a request came with."""

    id: str
    name: str
    grants: tuple[Grant, ...]
    expires: str | None
    """RFC 3339, or ``None`` for a token that never expires."""

    @classmethod
    def from_json(cls, data: Any) -> "TokenSummary":
        return _parse(
            "token",
            data,
            lambda d: cls(
                id=d["id"],
                name=d["name"],
                grants=tuple(Grant.from_json(g) for g in d["grants"]),
                expires=d["expires"],
            ),
        )


@dataclass(frozen=True)
class Me:
    """``GET /api/v1/auth/me``: who the server thinks is calling."""

    user: str | None
    authenticated: bool
    server_admin: bool
    authentication_configured: bool
    token: TokenSummary | None

    @classmethod
    def from_json(cls, data: Any) -> "Me":
        return _parse(
            "caller",
            data,
            lambda d: cls(
                user=d["user"],
                authenticated=d["authenticated"],
                server_admin=d["server_admin"],
                authentication_configured=d["authentication_configured"],
                # Absent from a server older than API tokens (#194).
                token=None
                if d.get("token") is None
                else TokenSummary.from_json(d["token"]),
            ),
        )


@dataclass(frozen=True)
class SignedIn:
    """``POST /api/v1/auth/login``."""

    user: str
    server_admin: bool

    @classmethod
    def from_json(cls, data: Any) -> "SignedIn":
        return _parse(
            "sign-in",
            data,
            lambda d: cls(user=d["user"], server_admin=d["server_admin"]),
        )


@dataclass(frozen=True)
class Project:
    """One project as the caller sees it."""

    key: str
    """What the project is addressed by. ``ridal gui`` serves its one project
    as ``default``."""
    name: str
    description: str | None
    description_long_html: str | None
    archived: bool
    created_by: str | None
    member: bool
    member_count: int
    radargram_count: int | None
    role: Role
    """The caller's role here, including any API token's ceiling."""
    download: DownloadScope
    require_auth_to_read: bool

    @classmethod
    def from_json(cls, data: Any) -> "Project":
        return _parse(
            "project",
            data,
            lambda d: cls(
                key=d["key"],
                name=d["name"],
                description=d["description"],
                description_long_html=d["description_long_html"],
                archived=d["archived"],
                created_by=d["created_by"],
                member=d["member"],
                member_count=d["member_count"],
                radargram_count=d["radargram_count"],
                role=d["role"],
                download=d["download"],
                require_auth_to_read=d["require_auth_to_read"],
            ),
        )


@dataclass(frozen=True)
class Dataset:
    """One radargram as the server catalogues it."""

    radargram_id: str
    effective_label: str
    display_name: str | None
    group_name: str | None
    group_id: str | None
    relative_path: str
    processing_datetime: str
    """Verbatim from the file. With the radargram id it gives the revision id."""
    processing_datetime_display: str
    revision_id: str
    """Matches ``ridal.info(path)[0]["revision_id"]`` for the same file."""
    shape: tuple[int, int]
    """``(samples, traces)``."""
    track_length_m: float | None
    in_project: bool
    unlisted: bool
    line_count: int | None
    contributor_count: int | None

    @classmethod
    def from_json(cls, data: Any) -> "Dataset":
        def build(d: Mapping[str, Any]) -> "Dataset":
            samples, traces = d["shape"]
            return cls(
                radargram_id=d["radargram_id"],
                effective_label=d["effective_label"],
                display_name=d["display_name"],
                group_name=d["group_name"],
                group_id=d["group_id"],
                relative_path=d["relative_path"],
                processing_datetime=d["processing_datetime"],
                processing_datetime_display=d["processing_datetime_display"],
                revision_id=d["revision_id"],
                shape=(int(samples), int(traces)),
                track_length_m=d["track_length_m"],
                in_project=d["in_project"],
                unlisted=d["unlisted"],
                line_count=d["line_count"],
                contributor_count=d["contributor_count"],
            )

        return _parse("dataset", data, build)


@dataclass(frozen=True)
class Catalog:
    """``GET …/datasets``: the radargrams the caller may see."""

    datasets: tuple[Dataset, ...]
    warnings: tuple[str, ...]
    """Problems found while cataloguing, such as an unreadable file."""

    @classmethod
    def from_json(cls, data: Any) -> "Catalog":
        return _parse(
            "catalog",
            data,
            lambda d: cls(
                datasets=tuple(Dataset.from_json(e) for e in d["entries"]),
                warnings=tuple(d["warnings"]),
            ),
        )

    def by_id(self) -> dict[str, Dataset]:
        """The datasets keyed by radargram id."""
        return {dataset.radargram_id: dataset for dataset in self.datasets}

    def to_pandas(self) -> Any:
        """The datasets as a ``pandas.DataFrame``, one row each.

        Raises
        ------
        ImportError
            If pandas is not installed (``pip install ridal[geo]``).
        """
        try:
            import pandas as pd
        except ImportError as error:
            raise ImportError(
                "Catalog.to_pandas() needs pandas: pip install ridal[geo]"
            ) from error
        rows = [
            {
                **dataset.__dict__,
                "samples": dataset.shape[0],
                "traces": dataset.shape[1],
            }
            for dataset in self.datasets
        ]
        frame = pd.DataFrame(rows).drop(columns="shape", errors="ignore")
        return frame.set_index("radargram_id") if len(frame) else frame


@dataclass(frozen=True)
class Axes:
    """``GET …/datasets/{id}/axes``. An axis the file lacks is ``None``."""

    distance: np.ndarray | None
    """Along-track distance per trace, in metres."""
    twtt: np.ndarray | None
    """Two-way travel time per sample, in nanoseconds."""
    depth: np.ndarray | None
    """Depth per sample, in metres."""
    elevation: np.ndarray | None
    """Surface elevation per trace, in metres, as the file stores it."""

    @classmethod
    def from_json(cls, data: Any) -> "Axes":
        def axis(value: Any) -> np.ndarray | None:
            return None if value is None else np.asarray(value, dtype=np.float64)

        return _parse(
            "axes",
            data,
            lambda d: cls(
                distance=axis(d["distance"]),
                twtt=axis(d["twtt"]),
                depth=axis(d["depth"]),
                elevation=axis(d["elevation"]),
            ),
        )


@dataclass(frozen=True)
class InterpretationList:
    """``GET …/datasets/{id}/interpretations``."""

    radargram_id: str
    users: tuple[str, ...]
    """Everyone with a stored interpretation of this radargram."""
    writable: bool
    """Whether the caller may save picks here."""

    @classmethod
    def from_json(cls, data: Any) -> "InterpretationList":
        return _parse(
            "interpretation list",
            data,
            lambda d: cls(
                radargram_id=d["radargram_id"],
                users=tuple(d["users"]),
                writable=d["writable"],
            ),
        )


@dataclass(frozen=True)
class Interpretation:
    """One person's picks on one radargram, as a gprinterp document.

    The document is GeoJSON-like and kept as the server sent it; see the
    gprinterp specification for its fields.
    """

    radargram_id: str
    user: str
    document: dict[str, Any]
    etag: str | None
    """The version that was read. Writing it back with this ``etag`` is
    refused (:class:`~ridal.client.errors.PreconditionFailed`) if someone
    changed the document since."""


@dataclass(frozen=True)
class FeatureCollection:
    """GeoJSON from the server: tracks, or level 2 points.

    Attributes
    ----------
    data : dict
        The GeoJSON as the server sent it.
    crs : str
        The CRS of its geometry: ``EPSG:4326`` (WGS84) unless a level 2
        export was asked for in another, which the server records in the
        file's ``ridal.output_crs``.
    """

    data: dict[str, Any]
    crs: str

    @classmethod
    def from_json(cls, data: Any) -> "FeatureCollection":
        def build(d: Mapping[str, Any]) -> "FeatureCollection":
            if d["type"] != "FeatureCollection":
                raise ValueError(f"expected a FeatureCollection, got {d['type']!r}")
            provenance = d.get("ridal") or {}
            return cls(data=dict(d), crs=provenance.get("output_crs") or "EPSG:4326")

        return _parse("GeoJSON", data, build)

    def to_geopandas(self) -> Any:
        """The features as a ``geopandas.GeoDataFrame``, one row each, with
        their properties as columns and the CRS set.

        Raises
        ------
        ImportError
            If geopandas is not installed (``pip install ridal[geo]``).
        """
        try:
            import geopandas as gpd
        except ImportError as error:
            raise ImportError(
                "FeatureCollection.to_geopandas() needs geopandas: pip install ridal[geo]"
            ) from error
        return gpd.GeoDataFrame.from_features(self.data["features"], crs=self.crs)


# -- Changing things ----------------------------------------------------------


@dataclass(frozen=True)
class Document:
    """A JSON document the server versions, such as the layer vocabulary.

    Kept as the server sent it. Write it back with its ``etag`` so a change
    someone else made in the meantime is refused rather than overwritten.
    """

    data: dict[str, Any]
    etag: str | None


@dataclass(frozen=True)
class Added:
    """``POST …/datasets``: a radargram uploaded into the project."""

    radargram_id: str
    revision_id: str
    display_name: str | None
    group_name: str | None
    bytes: int
    archived_interpretations: int
    """Interpretations already archived under this id, from an earlier
    removal. Not zero means the id was reused."""

    @classmethod
    def from_json(cls, data: Any) -> "Added":
        return _parse(
            "upload",
            data,
            lambda d: cls(
                radargram_id=d["radargram_id"],
                revision_id=d["revision_id"],
                display_name=d["display_name"],
                group_name=d["group_name"],
                bytes=d["bytes"],
                archived_interpretations=d["archived_interpretations"],
            ),
        )


@dataclass(frozen=True)
class Saved:
    """``PUT …/interpretations/{user}``: an interpretation was saved."""

    radargram_id: str
    user: str
    version: str
    """The new version. Pass it as ``etag`` with the next save."""
    warnings: tuple[str, ...]

    @classmethod
    def from_json(cls, data: Any) -> "Saved":
        return _parse(
            "save",
            data,
            lambda d: cls(
                radargram_id=d["radargram_id"],
                user=d["user"],
                version=d["version"],
                warnings=tuple(d["warnings"]),
            ),
        )


@dataclass(frozen=True)
class Dropped:
    """A feature that did not fit on the new revision."""

    index: int
    """Its position in the document's ``features``."""
    id: str | None
    label: str | None

    @classmethod
    def from_json(cls, data: Any) -> "Dropped":
        return _parse(
            "dropped feature",
            data,
            lambda d: cls(index=d["index"], id=d["id"], label=d["label"]),
        )


@dataclass(frozen=True)
class Displacement:
    """How far carried coordinates moved, in traces and samples."""

    median_traces: float
    worst_traces: float
    median_samples: float
    worst_samples: float

    @classmethod
    def from_json(cls, data: Any) -> "Displacement":
        return _parse(
            "displacement",
            data,
            lambda d: cls(
                median_traces=d["median_traces"],
                worst_traces=d["worst_traces"],
                median_samples=d["median_samples"],
                worst_samples=d["worst_samples"],
            ),
        )


@dataclass(frozen=True)
class CarryReport:
    """What carrying picks onto another revision did, or would do."""

    severity: Severity
    from_revision: str | None
    to_revision: str
    x_anchor: str | None
    y_anchor: str | None
    kept: int
    dropped: tuple[Dropped, ...]
    moved: Displacement | None
    refusal: str | None
    headline: str
    #: What the axes cannot account for, such as one revision being
    #: migrated and the other not. Empty from servers that predate it.
    warnings: tuple[str, ...] = ()

    @classmethod
    def from_json(cls, data: Any) -> "CarryReport":
        return _parse("carry report", data, _carry_fields)


def _carry_fields(d: Mapping[str, Any]) -> CarryReport:
    return CarryReport(
        severity=d["severity"],
        from_revision=d["from_revision"],
        to_revision=d["to_revision"],
        x_anchor=d["x_anchor"],
        y_anchor=d["y_anchor"],
        kept=d["kept"],
        dropped=tuple(Dropped.from_json(item) for item in d["dropped"]),
        moved=None if d["moved"] is None else Displacement.from_json(d["moved"]),
        refusal=d["refusal"],
        headline=d["headline"],
        warnings=tuple(d.get("warnings", ())),
    )


@dataclass(frozen=True)
class DocumentConsequence:
    """What replacing a radargram would do to one person's picks."""

    user: str
    carry: CarryReport

    @classmethod
    def from_json(cls, data: Any) -> "DocumentConsequence":
        # The server sends the carry report's fields flattened beside `user`.
        return _parse(
            "document consequence",
            data,
            lambda d: cls(user=d["user"], carry=_carry_fields(d)),
        )


@dataclass(frozen=True)
class ShapeChange:
    """How the two revisions' grids compare."""

    from_traces: int
    from_samples: int
    to_traces: int
    to_samples: int
    changed: bool

    @classmethod
    def from_json(cls, data: Any) -> "ShapeChange":
        return _parse(
            "shape change",
            data,
            lambda d: cls(
                from_traces=d["from_traces"],
                from_samples=d["from_samples"],
                to_traces=d["to_traces"],
                to_samples=d["to_samples"],
                changed=d["changed"],
            ),
        )


@dataclass(frozen=True)
class ConsequenceReport:
    """Everything replacing a radargram would do, before it does any of it."""

    radargram_id: str
    from_revision: str
    to_revision: str
    revision_id_collision: bool
    """The new file has the same revision id as the current one, so nothing
    could tell them apart. Such a replace is refused when committed."""
    shape: ShapeChange | None
    outgoing_axes_kept: bool
    """Whether the current revision's mapping can be kept. Without it, picks
    drawn on it could never be shown again, and the replace is refused."""
    documents: tuple[DocumentConsequence, ...]
    worst: Severity
    """The worst severity across every interpretation."""
    headline: str

    @classmethod
    def from_json(cls, data: Any) -> "ConsequenceReport":
        return _parse(
            "consequence report",
            data,
            lambda d: cls(
                radargram_id=d["radargram_id"],
                from_revision=d["from_revision"],
                to_revision=d["to_revision"],
                revision_id_collision=d["revision_id_collision"],
                shape=None if d["shape"] is None else ShapeChange.from_json(d["shape"]),
                outgoing_axes_kept=d["outgoing_axes_kept"],
                documents=tuple(
                    DocumentConsequence.from_json(item) for item in d["documents"]
                ),
                worst=d["worst"],
                headline=d["headline"],
            ),
        )


@dataclass(frozen=True)
class Staged:
    """``POST …/datasets/{id}/replace``: a replacement waiting to be committed."""

    token: str
    bytes: int
    report: ConsequenceReport

    @classmethod
    def from_json(cls, data: Any) -> "Staged":
        return _parse(
            "staged replacement",
            data,
            lambda d: cls(
                token=d["token"],
                bytes=d["bytes"],
                report=ConsequenceReport.from_json(d["report"]),
            ),
        )


@dataclass(frozen=True)
class Replaced:
    """A committed replacement."""

    radargram_id: str
    from_revision: str
    to_revision: str

    @classmethod
    def from_json(cls, data: Any) -> "Replaced":
        return _parse(
            "replacement",
            data,
            lambda d: cls(
                radargram_id=d["radargram_id"],
                from_revision=d["from_revision"],
                to_revision=d["to_revision"],
            ),
        )


@dataclass(frozen=True)
class CarriedView:
    """Someone's picks as they would be drawn on the current revision."""

    report: CarryReport
    document: dict[str, Any] | None
    """The gprinterp document as drawn, or ``None`` when it cannot be
    carried."""
    etag: str | None
    """The version of the stored interpretation it was carried from."""


@dataclass(frozen=True)
class Promoted:
    """``POST …/promote``: carried picks adopted onto the current revision."""

    radargram_id: str
    user: str
    from_revision: str | None
    to_revision: str
    dropped: tuple[Dropped, ...]
    archived: str | None

    @classmethod
    def from_json(cls, data: Any) -> "Promoted":
        return _parse(
            "promotion",
            data,
            lambda d: cls(
                radargram_id=d["radargram_id"],
                user=d["user"],
                from_revision=d["from_revision"],
                to_revision=d["to_revision"],
                dropped=tuple(Dropped.from_json(item) for item in d["dropped"]),
                archived=d["archived"],
            ),
        )


# -- Processing on the server (#382) -------------------------------------------

JobStateName: TypeAlias = Literal[
    "queued", "running", "done", "failed", "cancelled", "timed_out"
]
StandingName: TypeAlias = Literal[
    "current", "stale", "older_processing", "not_processed", "no_raw_data"
]
OutcomeName: TypeAlias = Literal[
    "waiting",
    "processing",
    "committed",
    "already_current",
    "skipped",
    "failed",
    "interrupted",
]
FileRole: TypeAlias = Literal["header", "data", "coordinates"]


@dataclass(frozen=True)
class HashedFile:
    """A file the project stores, by name and content."""

    name: str
    blake3: str

    @classmethod
    def from_json(cls, data: Any) -> "HashedFile":
        return _parse("file", data, lambda d: cls(name=d["name"], blake3=d["blake3"]))


@dataclass(frozen=True)
class StoredFile:
    """``POST /raw``: a raw file stored in the project, or found there already.

    Pass it, in a list per recording, to :meth:`~ridal.client.Client.set_source`.
    """

    name: str
    blake3: str
    bytes: int

    @classmethod
    def from_json(cls, data: Any) -> "StoredFile":
        return _parse(
            "stored file",
            data,
            lambda d: cls(name=d["name"], blake3=d["blake3"], bytes=d["bytes"]),
        )


@dataclass(frozen=True)
class BundleFile:
    """One file of a recording, with what its reader uses it for."""

    role: FileRole
    name: str
    blake3: str

    @classmethod
    def from_json(cls, data: Any) -> "BundleFile":
        return _parse(
            "recording file",
            data,
            lambda d: cls(role=d["role"], name=d["name"], blake3=d["blake3"]),
        )


@dataclass(frozen=True)
class Bundle:
    """One recording of a source: its format and its files."""

    format: str
    files: tuple[BundleFile, ...]

    @classmethod
    def from_json(cls, data: Any) -> "Bundle":
        return _parse(
            "recording",
            data,
            lambda d: cls(
                format=d["format"],
                files=tuple(BundleFile.from_json(f) for f in d["files"]),
            ),
        )


@dataclass(frozen=True)
class SourceDem:
    """A source's link to one of the project's DEMs, with its file when linked."""

    id: str
    name: str
    blake3: str

    @classmethod
    def from_json(cls, data: Any) -> "SourceDem":
        return _parse(
            "DEM link",
            data,
            lambda d: cls(id=d["id"], name=d["name"], blake3=d["blake3"]),
        )


@dataclass(frozen=True)
class Source:
    """What a radargram is made from: its recordings in merge order, and
    optionally a CRS and a DEM."""

    bundles: tuple[Bundle, ...]
    crs: str | None
    dem: SourceDem | None

    @classmethod
    def from_json(cls, data: Any) -> "Source":
        return _parse(
            "source",
            data,
            lambda d: cls(
                bundles=tuple(Bundle.from_json(b) for b in d["bundles"]),
                crs=d["crs"],
                dem=None if d["dem"] is None else SourceDem.from_json(d["dem"]),
            ),
        )


@dataclass(frozen=True)
class NamedFormat:
    name: str
    description: str

    @classmethod
    def from_json(cls, data: Any) -> "NamedFormat":
        return _parse(
            "format",
            data,
            lambda d: cls(name=d["name"], description=d["description"]),
        )


@dataclass(frozen=True)
class MetadataSummary:
    """What a recording's header says."""

    samples: int
    sampling_frequency_mhz: float
    frequency_steps: int
    time_interval_s: float
    antenna_name: str
    antenna_mhz: float
    antenna_separation_m: float
    time_window_ns: float
    last_trace: int
    medium_velocity_m_per_ns: float

    @classmethod
    def from_json(cls, data: Any) -> "MetadataSummary":
        return _parse("header", data, lambda d: cls(**{k: d[k] for k in _fields(cls)}))


@dataclass(frozen=True)
class CentroidSummary:
    easting: float
    northing: float
    altitude: float

    @classmethod
    def from_json(cls, data: Any) -> "CentroidSummary":
        return _parse(
            "centroid", data, lambda d: cls(**{k: d[k] for k in _fields(cls)})
        )


@dataclass(frozen=True)
class CorrectionSummary:
    """How the elevations were corrected, and from what."""

    kind: str
    source: str | None

    @classmethod
    def from_json(cls, data: Any) -> "CorrectionSummary":
        return _parse(
            "correction", data, lambda d: cls(kind=d["kind"], source=d["source"])
        )


@dataclass(frozen=True)
class LocationSummary:
    """Where a recording was made, in the CRS it was projected into."""

    n_points: int
    crs: str
    start_time: str
    stop_time: str
    duration_s: float
    track_length_m: float
    altitude_min_m: float
    altitude_max_m: float
    centroid: CentroidSummary
    correction: CorrectionSummary

    @classmethod
    def from_json(cls, data: Any) -> "LocationSummary":
        def build(d: Mapping[str, Any]) -> "LocationSummary":
            plain = {
                k: d[k] for k in _fields(cls) if k not in ("centroid", "correction")
            }
            return cls(
                **plain,
                centroid=CentroidSummary.from_json(d["centroid"]),
                correction=CorrectionSummary.from_json(d["correction"]),
            )

        return _parse("location", data, build)


@dataclass(frozen=True)
class RelatedFiles:
    header: str
    data: str
    coordinates: str

    @classmethod
    def from_json(cls, data: Any) -> "RelatedFiles":
        return _parse(
            "related files",
            data,
            lambda d: cls(
                header=d["header"], data=d["data"], coordinates=d["coordinates"]
            ),
        )


@dataclass(frozen=True)
class Recording:
    """What one raw recording's header and positions say, as the server read
    them when its source was set."""

    input: str
    format: NamedFormat
    metadata: MetadataSummary
    location: LocationSummary
    related_files: RelatedFiles

    @classmethod
    def from_json(cls, data: Any) -> "Recording":
        return _parse(
            "recording",
            data,
            lambda d: cls(
                input=d["input"],
                format=NamedFormat.from_json(d["format"]),
                metadata=MetadataSummary.from_json(d["metadata"]),
                location=LocationSummary.from_json(d["location"]),
                related_files=RelatedFiles.from_json(d["related_files"]),
            ),
        )


@dataclass(frozen=True)
class SourceView:
    """``GET``/``PUT /sources/{radargram_id}``: a radargram's source."""

    radargram_id: str
    source: Source
    recordings: tuple[Recording, ...]
    """What each recording says, in merge order. Only setting a source, or
    linking a DEM, reads them; otherwise empty."""
    etag: str | None = None
    """The source's version, for a conditional change."""

    @classmethod
    def from_json(cls, data: Any, etag: str | None = None) -> "SourceView":
        return _parse(
            "source",
            data,
            lambda d: cls(
                radargram_id=d["radargram_id"],
                source=Source.from_json(d["source"]),
                recordings=tuple(Recording.from_json(r) for r in d["recordings"]),
                etag=etag,
            ),
        )


@dataclass(frozen=True)
class Dem:
    """One of the project's DEMs, and the radargrams linked to it."""

    id: str
    name: str
    file: HashedFile
    by: str
    at: str
    """When its file was uploaded, RFC 3339."""
    bytes: int
    radargram_ids: tuple[str, ...]
    etag: str | None = None

    @classmethod
    def from_json(cls, data: Any, etag: str | None = None) -> "Dem":
        return _parse(
            "DEM",
            data,
            lambda d: cls(
                id=d["id"],
                name=d["name"],
                file=HashedFile.from_json(d["file"]),
                by=d["by"],
                at=d["at"],
                bytes=d["bytes"],
                radargram_ids=tuple(d["radargram_ids"]),
                etag=etag,
            ),
        )


@dataclass(frozen=True)
class StepProgress:
    """Which step a processing job is on: ``index`` of ``of``, from 0."""

    index: int
    of: int
    step: str

    @classmethod
    def from_json(cls, data: Any) -> "StepProgress":
        return _parse(
            "step",
            data,
            lambda d: cls(index=d["index"], of=d["of"], step=d["step"]),
        )


@dataclass(frozen=True)
class Draft:
    """A radargram processed on the server from its raw data, to look at
    before it becomes the radargram."""

    job: int
    radargram_id: str
    base_revision: str | None
    """The revision served when it was asked for; ``None`` for a new id."""
    state: JobStateName
    revision_id: str | None
    """Set once ``done``."""
    message: str | None
    """Why it ``failed``."""
    step: StepProgress | None
    writing: bool
    """Every step has run and the file is being written."""

    @property
    def finished(self) -> bool:
        return self.state not in ("queued", "running")

    @classmethod
    def from_json(cls, data: Any) -> "Draft":
        return _parse(
            "draft",
            data,
            lambda d: cls(
                job=d["job"],
                radargram_id=d["radargram_id"],
                base_revision=d["base_revision"],
                state=d["state"],
                revision_id=d.get("revision_id"),
                message=d.get("message"),
                step=None if d["step"] is None else StepProgress.from_json(d["step"]),
                writing=d["writing"],
            ),
        )


@dataclass(frozen=True)
class ProfileVersion:
    version: int
    steps: tuple[str, ...]
    """Canonical, with every argument written out."""
    medium_velocity: float
    at: str
    by: str

    @classmethod
    def from_json(cls, data: Any) -> "ProfileVersion":
        return _parse(
            "profile version",
            data,
            lambda d: cls(
                version=d["version"],
                steps=tuple(d["steps"]),
                medium_velocity=d["medium_velocity"],
                at=d["at"],
                by=d["by"],
            ),
        )


@dataclass(frozen=True)
class Profile:
    """A processing profile: steps and a velocity that many radargrams
    share, with every version it has had."""

    id: str
    name: str
    description: str
    versions: tuple[ProfileVersion, ...]
    """Oldest first."""
    current_version: int
    members: tuple[str, ...]
    """The radargrams in it."""
    etag: str | None = None

    @property
    def current(self) -> ProfileVersion:
        (current,) = [v for v in self.versions if v.version == self.current_version]
        return current

    @classmethod
    def from_json(cls, data: Any, etag: str | None = None) -> "Profile":
        return _parse(
            "processing profile",
            data,
            lambda d: cls(
                id=d["id"],
                name=d["name"],
                description=d["description"],
                versions=tuple(ProfileVersion.from_json(v) for v in d["versions"]),
                current_version=d["current_version"],
                members=tuple(d["members"]),
                etag=etag,
            ),
        )


@dataclass(frozen=True)
class Assignment:
    """Which processing profile a radargram is in, and its own steps, which
    run first and may only select traces and samples."""

    profile: str
    pre_steps: tuple[str, ...]

    @classmethod
    def from_json(cls, data: Any) -> "Assignment":
        return _parse(
            "assignment",
            data,
            lambda d: cls(profile=d["profile"], pre_steps=tuple(d["pre_steps"])),
        )


@dataclass(frozen=True)
class RadargramStatus:
    """Where a radargram in a profile stands against it."""

    radargram_id: str
    profile: str
    state: StandingName
    profile_version: int | None
    """The version it matches, when it matches one."""
    custom: bool | None
    """When ``stale``: processed with steps or a velocity that match no
    version of the profile."""
    reasons: tuple[str, ...] | None
    """When ``stale`` or ``older_processing``: why."""

    @classmethod
    def from_json(cls, data: Any) -> "RadargramStatus":
        return _parse(
            "status",
            data,
            lambda d: cls(
                radargram_id=d["radargram_id"],
                profile=d["profile"],
                state=d["state"],
                profile_version=d.get("profile_version"),
                custom=d.get("custom"),
                reasons=None if "reasons" not in d else tuple(d["reasons"]),
            ),
        )


@dataclass(frozen=True)
class PicksNotCarried:
    """Someone's picks that an applied batch item did not carry whole."""

    user: str
    severity: Severity
    dropped: int

    @classmethod
    def from_json(cls, data: Any) -> "PicksNotCarried":
        return _parse(
            "picks not carried",
            data,
            lambda d: cls(user=d["user"], severity=d["severity"], dropped=d["dropped"]),
        )


@dataclass(frozen=True)
class BatchItem:
    """One radargram in a batch, and how it went."""

    radargram_id: str
    outcome: OutcomeName
    job: int | None
    """While ``processing``."""
    revision_id: str | None
    """Once ``committed``."""
    picks_not_carried: tuple[PicksNotCarried, ...] | None
    """Once ``committed``: whose picks did not come across whole."""
    reason: str | None
    """Why it was ``skipped``."""
    message: str | None
    """Why it ``failed``."""

    @classmethod
    def from_json(cls, data: Any) -> "BatchItem":
        return _parse(
            "batch item",
            data,
            lambda d: cls(
                radargram_id=d["radargram_id"],
                outcome=d["outcome"],
                job=d.get("job"),
                revision_id=d.get("revision_id"),
                picks_not_carried=None
                if "picks_not_carried" not in d
                else tuple(
                    PicksNotCarried.from_json(p) for p in d["picks_not_carried"]
                ),
                reason=d.get("reason"),
                message=d.get("message"),
            ),
        )


@dataclass(frozen=True)
class Batch:
    """A profile's out-of-date radargrams, reprocessed and applied without
    review."""

    id: str
    profile: str
    version: int
    """The profile version it processes with."""
    started_by: str
    started_at: str
    finished_at: str | None
    items: tuple[BatchItem, ...]
    seen_at: str | None
    """When whoever started it dismissed its report."""

    @property
    def finished(self) -> bool:
        return self.finished_at is not None or all(
            item.outcome not in ("waiting", "processing") for item in self.items
        )

    @classmethod
    def from_json(cls, data: Any) -> "Batch":
        return _parse(
            "batch",
            data,
            lambda d: cls(
                id=d["id"],
                profile=d["profile"],
                version=d["version"],
                started_by=d["started_by"],
                started_at=d["started_at"],
                finished_at=d["finished_at"],
                items=tuple(BatchItem.from_json(i) for i in d["items"]),
                seen_at=d["seen_at"],
            ),
        )


def _fields(cls: type) -> tuple[str, ...]:
    """The names of a dataclass's fields, for records that are flat lists of
    numbers and strings."""
    return tuple(f.name for f in dataclasses.fields(cls))
