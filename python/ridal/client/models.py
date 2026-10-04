"""What the server answers, as plain records.

Each class mirrors one schema in the server's OpenAPI description
(``docs/reference/openapi.json``, served at ``/api/v1/openapi.json``), field
for field; a test holds them to it. Fields the server sends as ``null`` are
``None`` here.
"""

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
