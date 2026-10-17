"""The public :class:`Client`."""

import concurrent.futures
import dataclasses
import json
import os
import threading
import time
import warnings
from collections.abc import Callable, Collection, Iterable, Mapping
from os import PathLike
from pathlib import Path
from types import TracebackType
from typing import Any, Final, Literal, Self, TypeAlias, TypeVar

import ridal
from ridal import _ridal
from ridal.client import _http, errors, models
from ridal.client import plan as planning
from ridal.client.progress import Progress

#: Where the client looks for an API token when none is passed.
TOKEN_VARIABLE: Final = "RIDAL_TOKEN"

#: The project key ``ridal gui`` serves its one project under.
DEFAULT_PROJECT: Final = "default"

#: How many requests :meth:`Client.plan` and :meth:`Client.download_radargrams`
#: have in flight at once by default. Enough to hide the round trips on a
#: slow link without many large downloads competing for the same bandwidth.
DEFAULT_WORKERS: Final = 4

Spacing: TypeAlias = Literal["auto", "per-trace", "vertices"] | float

_T = TypeVar("_T")
_R = TypeVar("_R")


class Client:
    """A connection to a Ridal server, acting in one project.

    Parameters
    ----------
    url : str
        The server's root, such as ``https://ridal.example.org`` or the
        ``http://127.0.0.1:PORT`` that ``ridal gui`` prints.
    project : str, default "default"
        The project's key. ``ridal gui`` serves its one project as
        ``default``; on a site, :meth:`projects` lists the keys.
    token : str, optional
        An API token. Defaults to the ``RIDAL_TOKEN`` environment variable.
        Without either, the client is anonymous until :meth:`login`.
    timeout : float, default 60
        Seconds to wait for the server to connect or to send the next bytes.

    Examples
    --------
    >>> with Client("https://ridal.example.org", project="glac") as client:  # doctest: +SKIP
    ...     for dataset in client.catalog().datasets:
    ...         print(dataset.radargram_id, dataset.revision_id)
    """

    def __init__(
        self,
        url: str,
        *,
        project: str = DEFAULT_PROJECT,
        token: str | None = None,
        timeout: float = 60.0,
        _transport: Any = None,
    ) -> None:
        self.project = project
        self._http = _http.Http(
            url,
            token=token
            if token is not None
            else os.environ.get(TOKEN_VARIABLE) or None,
            timeout=timeout,
            transport=_transport,
        )

    @property
    def url(self) -> str:
        return self._http.base

    def close(self) -> None:
        """Close the connection. The client cannot be used afterwards."""
        self._http.close()

    def __enter__(self) -> Self:
        return self

    def __exit__(
        self,
        exc_type: type[BaseException] | None,
        exc: BaseException | None,
        traceback: TracebackType | None,
    ) -> None:
        self.close()

    def __repr__(self) -> str:
        return f"Client({self.url!r}, project={self.project!r})"

    # -- Paths ---------------------------------------------------------------

    def _project_path(self, *segments: str) -> str:
        tail = "/".join(_http.quote(segment) for segment in segments)
        return f"/api/v1/projects/{_http.quote(self.project)}/{tail}"

    def _dataset_path(self, radargram_id: str, *segments: str) -> str:
        return self._project_path("datasets", radargram_id, *segments)

    # -- The server and the caller -------------------------------------------

    def health(self) -> models.Health:
        """Whether the server is up, and which Ridal version it runs.

        Needs no sign-in, so it is a cheap first check.
        """
        return models.Health.from_json(self._http.json("GET", "/api/v1/health"))

    def me(self) -> models.Me:
        """Who the server thinks is calling, and with which API token."""
        return models.Me.from_json(self._http.json("GET", "/api/v1/auth/me"))

    def login(self, name: str, password: str) -> models.SignedIn:
        """Sign in with a password, for a session kept by this client.

        A script should prefer an API token, which can be limited to the
        projects and roles it needs. A server reachable over a network
        refuses a password without HTTPS.
        """
        return models.SignedIn.from_json(
            self._http.json(
                "POST", "/api/v1/auth/login", json={"name": name, "password": password}
            )
        )

    def logout(self) -> None:
        """End the session started by :meth:`login`."""
        self._http.json("POST", "/api/v1/auth/logout")

    def projects(self) -> tuple[models.Project, ...]:
        """The projects the caller may act in."""
        answer = self._http.json("GET", "/api/v1/projects")
        return tuple(models.Project.from_json(entry) for entry in answer["projects"])

    # -- The catalog ---------------------------------------------------------

    def catalog(self) -> models.Catalog:
        """Every radargram in the project the caller may see."""
        return models.Catalog.from_json(
            self._http.json("GET", self._project_path("datasets"))
        )

    def dataset(self, radargram_id: str) -> models.Dataset:
        """One radargram's summary."""
        return models.Dataset.from_json(
            self._http.json("GET", self._dataset_path(radargram_id))
        )

    def axes(self, radargram_id: str) -> models.Axes:
        """A radargram's distance, travel time, depth and elevation axes."""
        return models.Axes.from_json(
            self._http.json("GET", self._dataset_path(radargram_id, "axes"))
        )

    # -- Interpretations -----------------------------------------------------

    def interpretations(self, radargram_id: str) -> models.InterpretationList:
        """Who has interpreted a radargram, and whether the caller may."""
        return models.InterpretationList.from_json(
            self._http.json("GET", self._dataset_path(radargram_id, "interpretations"))
        )

    def interpretation_summaries(self) -> models.InterpretationSummaryList:
        """Every stored interpretation in the project, as statistics.

        An ``operator`` gets everyone's; anyone else only their own. No
        picks are returned, only counts and coverage.
        """
        return models.InterpretationSummaryList.from_json(
            self._http.json("GET", self._project_path("interpretations"))
        )

    def interpretation(self, radargram_id: str, user: str) -> models.Interpretation:
        """One person's picks, as a gprinterp document.

        Reading one's own needs no download scope; someone else's needs
        ``picks``.
        """
        response = self._http.request(
            "GET", self._dataset_path(radargram_id, "interpretations", user)
        )
        try:
            document = response.json()
        except ValueError as error:
            raise errors.ProtocolError("the interpretation is not JSON") from error
        return models.Interpretation(
            radargram_id=radargram_id,
            user=user,
            document=document,
            etag=response.headers.get("ETag"),
        )

    # -- Downloads -----------------------------------------------------------

    def download_radargram(
        self,
        radargram_id: str,
        destination: str | PathLike[str],
        *,
        progress: Progress | None = None,
    ) -> Path:
        """Download the processed NetCDF file.

        Parameters
        ----------
        radargram_id : str
        destination : path-like
            A file to write, or a directory to write ``<radargram_id>.nc``
            into. Written to a temporary file first and moved into place when
            complete.
        progress : callable, optional
            Called with a :class:`~ridal.client.ProgressEvent` as bytes
            arrive. :func:`~ridal.client.tqdm_progress` draws a bar.

        Returns
        -------
        pathlib.Path
            Where the file was written.
        """
        return self._http.download(
            self._dataset_path(radargram_id, "download"),
            Path(destination),
            label=f"{radargram_id}.nc",
            progress=progress,
        )

    def download_radargrams(
        self,
        radargram_ids: Iterable[str],
        directory: str | PathLike[str],
        *,
        workers: int = DEFAULT_WORKERS,
        progress: Progress | None = None,
    ) -> tuple[Path, ...]:
        """Download several processed NetCDF files at once.

        Parameters
        ----------
        radargram_ids : iterable of str
        directory : path-like
            Where to write each ``<radargram_id>.nc``. Created if missing.
        workers : int, default 4
            How many downloads run at the same time.
        progress : callable, optional
            As for :meth:`download_radargram`, called from several threads;
            each event's ``label`` says which file it is about.
            :func:`~ridal.client.tqdm_progress` draws one bar per file.

        Returns
        -------
        tuple of pathlib.Path
            Where each file was written, in the order of ``radargram_ids``.

        Raises
        ------
        RidalError
            The first download that failed. Those not yet started are not
            started, and those under way finish first. Each file is moved
            into place only when complete, so the finished ones are kept and
            none is left half written.
        """
        target = Path(directory)
        target.mkdir(parents=True, exist_ok=True)
        return tuple(
            _map(
                lambda radargram_id: self.download_radargram(
                    radargram_id, target, progress=progress
                ),
                radargram_ids,
                workers,
            )
        )

    def track(self, radargram_id: str) -> models.FeatureCollection:
        """A radargram's track in WGS84, one feature per continuous segment.

        ``.to_geopandas()`` turns it into a ``GeoDataFrame``. Needs the
        ``all`` download scope.
        """
        return models.FeatureCollection.from_json(
            self._http.json("GET", self._dataset_path(radargram_id, "track.geojson"))
        )

    def tracks(self) -> models.FeatureCollection:
        """Every track in the project, in WGS84. Needs the ``all`` download
        scope."""
        return models.FeatureCollection.from_json(
            self._http.json("GET", self._project_path("catalog", "track.geojson"))
        )

    def level2(
        self,
        radargram_id: str,
        *,
        user: str | None = None,
        derived: bool = False,
        spacing: Spacing | None = None,
        crs: str | None = None,
    ) -> models.FeatureCollection:
        """Level 2 points as GeoJSON in memory; :meth:`download_level2`
        writes them to a file instead, and takes the same parameters.

        ``.to_geopandas()`` turns the answer into a ``GeoDataFrame`` in the
        CRS the points were asked for.
        """
        path, params = self._level2_request(radargram_id, user, derived, spacing, crs)
        return models.FeatureCollection.from_json(
            self._http.json("GET", path, params={**params, "format": "geojson"})
        )

    def download_level2(
        self,
        radargram_id: str,
        destination: str | PathLike[str],
        *,
        user: str | None = None,
        derived: bool = False,
        spacing: Spacing | None = None,
        format: Literal["geojson", "csv"] = "geojson",
        crs: str | None = None,
        progress: Progress | None = None,
    ) -> Path:
        """Download level 2 points: picked or derived layers with coordinates.

        Parameters
        ----------
        radargram_id : str
        destination : path-like
            A file, or a directory to use the server's file name in.
        user : str, optional
            Whose picks. Defaults to the caller's own. Ignored with
            ``derived=True``.
        derived : bool, default False
            Export the project's derived layers instead of someone's picks.
        spacing : {"auto", "per-trace", "vertices"} or float, optional
            Point spacing, or a distance in metres. The server's default is
            ``auto``.
        format : {"geojson", "csv"}, default "geojson"
        crs : str, optional
            GeoJSON coordinates: WGS84 when omitted, ``"native"`` for the
            radargram's own projected CRS, or any CRS PROJ accepts. CSV
            always carries both.
        progress : callable, optional

        Returns
        -------
        pathlib.Path
        """
        path, params = self._level2_request(radargram_id, user, derived, spacing, crs)
        return self._http.download(
            path,
            Path(destination),
            params={**params, "format": format},
            label=f"{radargram_id}.{'csv' if format == 'csv' else 'geojson'}",
            progress=progress,
        )

    def _level2_request(
        self,
        radargram_id: str,
        user: str | None,
        derived: bool,
        spacing: Spacing | None,
        crs: str | None,
    ) -> tuple[str, dict[str, str]]:
        """The path and query of a level 2 export, without its format."""
        params: dict[str, str] = {}
        if spacing is not None:
            params["spacing"] = str(spacing)
        if crs is not None:
            params["crs"] = crs
        if derived:
            return self._dataset_path(radargram_id, "derived", "level2"), params
        who = user if user is not None else self._caller_name()
        return self._dataset_path(
            radargram_id, "interpretations", who, "level2"
        ), params

    def download_derived(
        self,
        radargram_id: str,
        destination: str | PathLike[str],
        *,
        progress: Progress | None = None,
    ) -> Path:
        """Download every derived item along a radargram as a long-format CSV."""
        return self._http.download(
            self._dataset_path(radargram_id, "derived"),
            Path(destination),
            label=f"{radargram_id}-derived.csv",
            progress=progress,
        )

    def figure(
        self,
        radargram_id: str,
        destination: str | PathLike[str],
        spec: Mapping[str, Any] | str | PathLike[str] | None = None,
    ) -> Path:
        """Render an annotated figure of a radargram, as the server sees it.

        The same figure ``ridal render --figure`` makes of the file, drawn by
        the server: with the viewer's contrast, and with the picks and
        derived layers this account may see.

        Parameters
        ----------
        radargram_id : str
        destination : path-like
            The file to write. Its extension picks the format: ``.svg`` or
            ``.png``. Written to a temporary file first and moved into place
            when complete.
        spec : mapping or path-like, optional
            The figure spec, as a mapping or the path to a JSON file holding
            one; see the rendering guide for its fields. Without one, the
            whole radargram with the default axes.

        Returns
        -------
        pathlib.Path
            Where the figure was written. A spec with ``"axes": false`` also
            writes its sidecar beside it, with a ``.json`` extension.

        Raises
        ------
        ValueError
            If ``destination`` is neither ``.svg`` nor ``.png``.
        ridal.client.Forbidden
            If the spec asks for picks or derived layers this account may
            not see.
        ridal.client.Conflict
            If the spec names a revision the server no longer serves.

        Warns
        -----
        ridal.client.FigureWarning
            For picks that could not be drawn as stored.
        """
        target = Path(destination)
        suffix = target.suffix.lower()
        if suffix not in {".svg", ".png"}:
            raise ValueError(
                f"A figure is written as .svg or .png, not {target.name!r}."
            )
        if spec is None:
            body: Any = {}
        elif isinstance(spec, Mapping):
            body = dict(spec)
        else:
            body = json.loads(Path(spec).read_text(encoding="utf-8"))
        path = self._dataset_path(radargram_id, "figure")

        response = self._http.request(
            "POST", path, params={"format": suffix[1:]}, json=body
        )
        with _http.atomic_file(target) as file:
            file.write(response.content)
        for message in json.loads(
            response.headers.get("X-Ridal-Figure-Warnings", "[]")
        ):
            warnings.warn(message, errors.FigureWarning, stacklevel=2)

        if body.get("axes", True) is False:
            sidecar = self._http.request(
                "POST", path, params={"format": "json"}, json=body
            )
            with _http.atomic_file(target.with_suffix(".json")) as file:
                file.write(sidecar.content)
        return target

    # -- Changing the catalog ------------------------------------------------

    def upload(
        self, path: str | PathLike[str], *, progress: Progress | None = None
    ) -> models.Added:
        """Add a processed radargram to the project. Needs ``operator``.

        Refused (:class:`~ridal.client.errors.Conflict`) when the project
        already has a radargram with this id; replace it instead.
        """
        source = Path(path)
        return models.Added.from_json(
            self._http.upload(
                "POST",
                self._project_path("datasets"),
                source,
                params={"filename": source.name},
                label=source.name,
                progress=progress,
            )
        )

    def preflight(self, path: str | PathLike[str]) -> models.ConsequenceReport:
        """What replacing a radargram with a local file would do, from the
        file's axes alone. Nothing is uploaded. Needs ``operator``.

        The answer is the report staging the file would give. It is advice,
        not a reservation: the radargram or its picks may change before a
        replace.
        """
        body = _ridal._preflight_body(Path(path))
        return models.ConsequenceReport.from_json(
            self._http.json(
                "POST",
                self._dataset_path(body["radargram_id"], "replace", "preflight"),
                json=body,
            )
        )

    def stage_replacement(
        self, path: str | PathLike[str], *, progress: Progress | None = None
    ) -> models.Staged:
        """Upload a new revision of a radargram without installing it.

        The answer carries the report and a token to
        :meth:`commit_replacement` or :meth:`discard_replacement` it with.
        A staged file counts against the project's size limit until then,
        and is swept after six hours.
        """
        source = Path(path)
        (local,) = ridal.info(source)
        if local["radargram_id"] is None:
            raise ValueError(
                f"{source} cannot replace anything: {local['reprocess_reason']}"
            )
        return models.Staged.from_json(
            self._http.upload(
                "POST",
                self._dataset_path(local["radargram_id"], "replace"),
                source,
                params={"filename": source.name},
                label=source.name,
                progress=progress,
            )
        )

    def commit_replacement(self, radargram_id: str, token: str) -> models.Replaced:
        """Install a staged replacement."""
        return models.Replaced.from_json(
            self._http.json("POST", self._dataset_path(radargram_id, "replace", token))
        )

    def discard_replacement(self, radargram_id: str, token: str) -> None:
        """Give a staged replacement back, freeing its space."""
        self._http.request("DELETE", self._dataset_path(radargram_id, "replace", token))

    def replace(
        self,
        path: str | PathLike[str],
        *,
        allow: Collection[models.Severity] = planning.DEFAULT_ALLOW,
        progress: Progress | None = None,
    ) -> models.Replaced:
        """Replace a radargram with a new revision, if the picks on it allow.

        Stages the file, reads the report, and commits only if every
        interpretation would be at most as affected as ``allow`` permits
        (by default, shown unmoved). Otherwise the staged file is discarded
        and :class:`ReplacementRefused` raised with the report.
        """
        staged = self.stage_replacement(path, progress=progress)
        radargram_id = staged.report.radargram_id
        reason = planning.acceptable(staged.report, allow)
        if reason is not None:
            self.discard_replacement(radargram_id, staged.token)
            raise errors.ReplacementRefused(reason, staged.report)
        return self.commit_replacement(radargram_id, staged.token)

    def plan(
        self,
        paths: Iterable[str | PathLike[str]],
        *,
        workers: int = DEFAULT_WORKERS,
    ) -> planning.Plan:
        """Sort local radargrams against the project, without changing it.

        See :mod:`ridal.client.plan` for what each status means. Reads only
        local identities, the catalog and who has interpreted what, and asks
        the server's preflight about files that would replace picked
        radargrams. Replacing needs ``operator``, and so does the preflight.

        Up to ``workers`` files are looked at at the same time; the records
        keep the order of ``paths``.
        """
        served = self.catalog().by_id()
        records = _map(lambda path: self._plan_one(Path(path), served), paths, workers)
        return planning.Plan(tuple(records))

    def _plan_one(
        self, path: Path, served: dict[str, models.Dataset]
    ) -> planning.Record:
        """One file's record in a :meth:`plan`."""
        (local,) = ridal.info(path)
        radargram_id, revision_id = local["radargram_id"], local["revision_id"]
        if radargram_id is None:
            return planning.Record(
                path, "legacy", None, None, None, reason=local["reprocess_reason"]
            )
        current = served.get(radargram_id)
        if current is None:
            return planning.Record(path, "new", radargram_id, revision_id, None)
        served_revision = current.revision_id
        if served_revision == revision_id:
            return planning.Record(
                path, "unchanged", radargram_id, revision_id, served_revision
            )
        if not self.interpretations(radargram_id).users:
            return planning.Record(
                path, "safe", radargram_id, revision_id, served_revision
            )
        return planning.Record(
            path,
            "risky",
            radargram_id,
            revision_id,
            served_revision,
            report=self.preflight(path),
        )

    def apply(
        self,
        planned: planning.Plan,
        *,
        allow: Collection[models.Severity] = planning.DEFAULT_ALLOW,
        progress: Progress | None = None,
        stop_on_outage: bool = True,
    ) -> tuple[planning.Outcome, ...]:
        """Upload the new files of a plan and replace the changed ones.

        One file at a time: a replacement is staged, checked against
        ``allow`` on the server's own report for the uploaded file, and
        committed or discarded before the next begins. A file the server
        refuses is reported in its outcome and the rest continue.

        When the server stops answering (a
        :class:`~ridal.client.errors.TransportError` or
        :class:`~ridal.client.errors.Unavailable`), that file fails and the
        files after it are ``not_attempted`` rather than each tried against
        a server that is not there. Planning again marks what was done as
        ``unchanged``, so applying that plan carries on where this stopped.
        ``stop_on_outage=False`` tries every file regardless.

        Unlike :meth:`plan`, this does not run files side by side. Uploads
        to the server do not wait for each other, but the room left in the
        project is measured again as each one finishes: several at once
        could each pass the check made when they began and then be refused,
        and staged replacements would pile up against the size limit.
        """
        outcomes = []
        outage: str | None = None
        for record in planned.records:
            if record.status in ("unchanged", "legacy"):
                detail = record.reason or "the server already has this revision"
                outcomes.append(planning.Outcome(record, "skipped", detail))
                continue
            if outage is not None:
                outcomes.append(planning.Outcome(record, "not_attempted", outage))
                continue
            try:
                if record.status == "new":
                    added = self.upload(record.path, progress=progress)
                    outcomes.append(
                        planning.Outcome(record, "uploaded", "added", added=added)
                    )
                    continue
                staged = self.stage_replacement(record.path, progress=progress)
                radargram_id = staged.report.radargram_id
                reason = planning.acceptable(staged.report, allow)
                if reason is not None:
                    self.discard_replacement(radargram_id, staged.token)
                    outcomes.append(
                        planning.Outcome(
                            record, "skipped", reason, report=staged.report
                        )
                    )
                    continue
                replaced = self.commit_replacement(radargram_id, staged.token)
                outcomes.append(
                    planning.Outcome(
                        record,
                        "replaced",
                        staged.report.headline,
                        replaced=replaced,
                        report=staged.report,
                    )
                )
            except (errors.TransportError, errors.Unavailable) as error:
                outcomes.append(planning.Outcome(record, "failed", str(error)))
                if stop_on_outage:
                    outage = (
                        f"not tried: the server stopped answering at {record.path.name}"
                    )
            except errors.RidalError as error:
                outcomes.append(planning.Outcome(record, "failed", str(error)))
        return tuple(outcomes)

    # -- Changing interpretations ---------------------------------------------

    def interpretation_template(self, radargram_id: str) -> dict[str, Any]:
        """An empty gprinterp document for picks on the current revision.

        It names the radargram and its current revision in ``source``, and
        carries the revision's axes in ``coordinates.axes``, as the browser
        writes them. Add features to it and :meth:`save_interpretation` it.
        Picks saved without those axes cannot be carried onto a reprocessed
        revision later.
        """
        answer = self._http.json(
            "GET", self._dataset_path(radargram_id, "axes", "gprinterp")
        )
        try:
            document: dict[str, Any] = {
                "key": answer["radargram_id"],
                "source": {
                    "id": answer["radargram_id"],
                    "revision_id": answer["revision_id"],
                },
                "features": [],
            }
            if answer["axes"] is not None:
                document["coordinates"] = {"axes": answer["axes"]}
        except (KeyError, TypeError) as error:
            raise errors.ProtocolError("the axes are not understood") from error
        return document

    def save_interpretation(
        self,
        radargram_id: str,
        document: dict[str, Any],
        *,
        etag: str | None = None,
        overwrite: bool = False,
    ) -> models.Saved:
        """Save the caller's picks on a radargram. Needs ``picker``.

        Parameters
        ----------
        radargram_id : str
        document : dict
            A gprinterp document.
        etag : str, optional
            The version this document was read at, from
            :meth:`interpretation`. The save is refused
            (:class:`~ridal.client.errors.PreconditionFailed`) if the stored
            document has changed since.
        overwrite : bool, default False
            Without an ``etag``, a save only creates a new interpretation and
            is refused if one exists. ``True`` replaces whatever is stored.
        """
        headers = _preconditions(etag, overwrite)
        return models.Saved.from_json(
            self._http.json(
                "PUT",
                self._dataset_path(
                    radargram_id, "interpretations", self._caller_name()
                ),
                json=document,
                headers=headers,
            )
        )

    def carried(self, radargram_id: str, user: str | None = None) -> models.CarriedView:
        """Someone's picks as they would be drawn on the current revision,
        when they were drawn on an earlier one, and what carrying them did."""
        who = user if user is not None else self._caller_name()
        response = self._http.request(
            "GET", self._dataset_path(radargram_id, "interpretations", who, "carried")
        )
        try:
            body = response.json()
            return models.CarriedView(
                report=models.CarryReport.from_json(body["report"]),
                document=body["document"],
                etag=response.headers.get("ETag"),
            )
        except (ValueError, KeyError, TypeError) as error:
            raise errors.ProtocolError("the carried view is not understood") from error

    def promote(self, radargram_id: str) -> models.Promoted:
        """Adopt the caller's carried picks onto the current revision.

        After a replace, picks drawn on the earlier revision are shown carried
        onto the new one, but stored as drawn. Promoting stores the carried
        view instead, so they are picks on the current revision. The
        document as drawn is archived first. Needs ``picker``, and only one's
        own picks.
        """
        view = self.carried(radargram_id)
        if view.document is None:
            raise ValueError(
                f"Nothing to promote: {view.report.refusal or view.report.headline}"
            )
        headers = {"If-Match": view.etag} if view.etag else {}
        return models.Promoted.from_json(
            self._http.json(
                "POST",
                self._dataset_path(
                    radargram_id, "interpretations", self._caller_name(), "promote"
                ),
                params={"onto": view.report.to_revision},
                json=view.document,
                headers=headers,
            )
        )

    # -- Project documents ----------------------------------------------------

    def layers(self) -> models.Document:
        """The project's layer vocabulary, with its version."""
        return self._document(self._project_path("layers"))

    def save_layers(
        self, data: dict[str, Any], *, etag: str | None, overwrite: bool = False
    ) -> models.Document:
        """Replace the layer vocabulary. Needs ``operator``.

        Pass the ``etag`` from :meth:`layers`, so a change someone else made
        in the meantime is refused
        (:class:`~ridal.client.errors.PreconditionFailed`) rather than
        overwritten. An ``etag`` of ``None`` means the project has no layers
        yet, and the save is refused if someone created them meanwhile;
        ``overwrite=True`` replaces whatever is stored.
        """
        return self._save_document(
            self._project_path("layers"), data, etag=etag, overwrite=overwrite
        )

    def derived(self) -> models.Document:
        """The derived items the caller may see, with their version."""
        return self._document(self._project_path("derived"))

    def save_derived(
        self, data: dict[str, Any], *, etag: str | None, overwrite: bool = False
    ) -> models.Document:
        """Replace the derived items, as the browser does, with the same
        ``etag`` and ``overwrite`` rules as :meth:`save_layers`. Project-wide
        items need ``operator``, and releasing one to everyone needs
        ``admin``."""
        return self._save_document(
            self._project_path("derived"), data, etag=etag, overwrite=overwrite
        )

    def _document(self, path: str) -> models.Document:
        return _as_document(path, self._http.request("GET", path))

    def _save_document(
        self, path: str, data: dict[str, Any], *, etag: str | None, overwrite: bool
    ) -> models.Document:
        headers = _preconditions(etag, overwrite)
        return _as_document(
            path, self._http.request("PUT", path, json=data, headers=headers)
        )

    # -- Processing on the server (#382) -------------------------------------

    def upload_raw(
        self, path: str | PathLike[str], *, progress: Progress | None = None
    ) -> models.StoredFile:
        """Store one raw file in the project, such as a recording's ``.rd3``.
        Needs ``operator``.

        Its name says what it is to its recording, so the file is sent under
        its own name. A file the project already has is not stored twice.
        """
        source = Path(path)
        return models.StoredFile.from_json(
            self._http.upload(
                "POST",
                self._project_path("raw"),
                source,
                params={"filename": source.name},
                label=source.name,
                progress=progress,
            )
        )

    def download_raw(
        self,
        blake3: str,
        destination: str | PathLike[str],
        *,
        progress: Progress | None = None,
    ) -> Path:
        """Download a stored raw file by its hash. Needs the ``all`` download
        scope. ``destination`` is a file, or a directory to write it into
        named by its hash."""
        return self._http.download(
            self._project_path("raw", blake3),
            Path(destination),
            label=blake3,
            progress=progress,
        )

    def sources(self) -> tuple[str, ...]:
        """The radargram ids the project has raw data for, processed yet or not."""
        answer = self._http.json("GET", self._project_path("sources"))
        return tuple(answer["radargram_ids"])

    def source(self, radargram_id: str) -> models.SourceView:
        """What a radargram is made from, with the version to change it at."""
        response = self._http.request(
            "GET", self._project_path("sources", radargram_id)
        )
        return models.SourceView.from_json(
            _json_of(response), etag=response.headers.get("ETag")
        )

    def set_source(
        self,
        radargram_id: str,
        recordings: Iterable[Iterable[models.StoredFile | models.HashedFile]],
        *,
        crs: str | None = None,
        dem: str | None = None,
        antenna_mhz: float | None = None,
        antenna_separation: float | None = None,
        names: models.SourceNames | None | Literal["keep"] = "keep",
        etag: str | None = None,
        only_new: bool = False,
    ) -> models.SourceView:
        """Set or switch what a radargram is made from. Needs ``operator``.

        Parameters
        ----------
        radargram_id : str
            Need not be processed yet: this is where processing a new one
            starts.
        recordings : iterable of iterables of StoredFile
            The recordings in the order they are merged, each the files it
            is made of, as :meth:`upload_raw` answered. Which file is the
            header, the data and the positions is read from their names.
        crs : str, optional
            An EPSG code such as ``"EPSG:32633"``. Without one, the project's
            (``[processing] crs``), else the UTM zone that suits the track.
        dem : str, optional
            The id of one of the project's DEMs (:meth:`dems`).
        antenna_mhz : float, optional
            The antenna's frequency in MHz, where the files do not say it or
            say it wrongly. Without it, it is read from the files. Every
            draft and batch made from the source uses it.
        antenna_separation : float, optional
            The distance between the antennas in m, likewise.
        names : SourceNames or None, optional
            What to call the radargram until it is served. ``"keep"`` (the
            default) keeps what the source had, and ``None`` removes it.
        etag : str, optional
            The source's version from :meth:`source`: the change is refused
            (:class:`~ridal.client.errors.PreconditionFailed`) if it has
            changed since.
        only_new : bool, default False
            Refuse (:class:`~ridal.client.errors.PreconditionFailed`) if the
            radargram has a source already, as creating from
            :meth:`plan_ingest` does.

        The server reads every recording before saving, and refuses
        recordings that cannot be merged, or a DEM that does not cover them.
        """
        body: dict[str, Any] = {
            "recordings": [
                [{"name": f.name, "blake3": f.blake3} for f in files]
                for files in recordings
            ],
            "crs": crs,
            "dem": dem,
            "antenna_mhz": antenna_mhz,
            "antenna_separation": antenna_separation,
        }
        if names != "keep":
            body["names"] = None if names is None else dataclasses.asdict(names)
        response = self._http.request(
            "PUT",
            self._project_path("sources", radargram_id),
            json=body,
            headers={"If-Match": etag}
            if etag
            else {"If-None-Match": "*"}
            if only_new
            else {},
        )
        return models.SourceView.from_json(
            _json_of(response), etag=response.headers.get("ETag")
        )

    def plan_ingest(
        self,
        recordings: Iterable[Iterable[models.StoredFile | models.HashedFile]],
        *,
        merge_threshold_seconds: float | None = None,
        excluded: Iterable[str] = (),
        joins: Iterable[str] = (),
        splits: Iterable[str] = (),
        id_pattern: str | None = None,
        display_name_pattern: str | None = None,
        ids: Mapping[str, str] | None = None,
        display_names: Mapping[str, str] | None = None,
        group_name: str | None = None,
        group_id: str | None = None,
        crs: str | None = None,
        dem: str | None = None,
        antennas: Mapping[str, models.AntennaOverride] | None = None,
        include_tracks: bool = True,
    ) -> models.IngestPlan:
        """Group uploaded recordings into the radargrams they would make, and
        name them. Needs ``operator``. Nothing is created: once
        :attr:`~ridal.client.IngestPlan.ok`, each output is a
        :meth:`set_source` with ``only_new=True``, and with the output's
        ``antenna_mhz_override`` and ``antenna_separation_override``.

        Parameters
        ----------
        recordings : iterable of iterables of StoredFile
            Every uploaded recording, in any order, each the files it is
            made of as :meth:`upload_raw` answered.
        merge_threshold_seconds : float, optional
            Recordings less than this far apart in time are merged, when
            their headers allow. ``None`` never merges.
        excluded, joins, splits : iterable of str
            Recordings by key (:attr:`PlannedRecording.key`): to leave out,
            to keep with the one before them whatever the gap, and to start
            a new radargram although they would merge.
        id_pattern, display_name_pattern : str, optional
            As ``ridal batch-process --id-pattern`` takes them. Without an
            id pattern, each output is named after its first file.
        ids, display_names : mapping of str to str, optional
            Typed over what the patterns give, by the key of the output's
            first recording.
        group_name, group_id : str, optional
            The catalog group, which ``{group}`` and ``{group_name}``
            render.
        crs, dem : str, optional
            As for :meth:`set_source`.
        antennas : mapping of str to AntennaOverride, optional
            Overrides by antenna, as the headers name it
            (:attr:`IngestPlan.antennas`): applied to every recording with
            that antenna before grouping and naming. An antenna whose name
            gives no frequency needs one here before its recordings can be
            planned.
        include_tracks : bool
            Whether to send each recording's simplified track.

        A plan with problems is still answered in full; its ``problems``
        say what stops it and where.
        """
        body: dict[str, Any] = {
            "recordings": [
                {"files": [{"name": f.name, "blake3": f.blake3} for f in files]}
                for files in recordings
            ],
            "merge_threshold_seconds": merge_threshold_seconds,
            "excluded": list(excluded),
            "joins": list(joins),
            "splits": list(splits),
            "id_pattern": id_pattern,
            "display_name_pattern": display_name_pattern,
            "ids": dict(ids or {}),
            "display_names": dict(display_names or {}),
            "group_name": group_name,
            "group_id": group_id,
            "crs": crs,
            "dem": dem,
            "antennas": {
                name: dataclasses.asdict(given)
                for name, given in (antennas or {}).items()
            },
            "include_tracks": include_tracks,
        }
        return models.IngestPlan.from_json(
            self._http.json("POST", self._project_path("ingest", "plan"), json=body)
        )

    def delete_source(self, radargram_id: str, *, etag: str | None = None) -> None:
        """Forget what a radargram is made from. Needs ``operator``."""
        self._http.request(
            "DELETE",
            self._project_path("sources", radargram_id),
            headers={"If-Match": etag} if etag else {},
        )

    def link_dem(
        self, radargram_id: str, dem: str | None, *, etag: str | None = None
    ) -> models.SourceView:
        """Link a radargram to one of the project's DEMs, or with ``None``
        unlink it, leaving its recordings as they are. Needs ``operator``.

        What is served was made otherwise, so it is stale afterwards.
        """
        response = self._http.request(
            "PUT",
            self._project_path("sources", radargram_id, "dem"),
            json={"dem": dem},
            headers={"If-Match": etag} if etag else {},
        )
        return models.SourceView.from_json(
            _json_of(response), etag=response.headers.get("ETag")
        )

    def dems(self) -> tuple[models.Dem, ...]:
        """The project's DEMs, and which radargrams use each."""
        answer = self._http.json("GET", self._project_path("dems"))
        return tuple(models.Dem.from_json(d) for d in answer["dems"])

    def dem(self, dem_id: str) -> models.Dem:
        """One of the project's DEMs."""
        response = self._http.request("GET", self._project_path("dems", dem_id))
        return models.Dem.from_json(
            _json_of(response), etag=response.headers.get("ETag")
        )

    def add_dem(
        self,
        dem_id: str,
        name: str,
        path: str | PathLike[str],
        *,
        replace: bool = False,
        progress: Progress | None = None,
    ) -> models.Dem:
        """Upload a DEM for the project. Needs ``operator``.

        Refused if the project already has a DEM with this id, unless
        ``replace`` is set; then its file is replaced, which the server
        refuses while any radargram links to it. GDAL must be able to read
        the file as a raster.
        """
        stored = self.upload_raw(path, progress=progress)
        return self._put_dem(
            dem_id,
            {"name": name, "file": {"name": stored.name, "blake3": stored.blake3}},
            headers={} if replace else {"If-None-Match": "*"},
        )

    def rename_dem(self, dem_id: str, name: str) -> models.Dem:
        """Give a DEM another name. Needs ``operator``."""
        return self._put_dem(dem_id, {"name": name}, headers={"If-Match": "*"})

    def _put_dem(
        self, dem_id: str, body: dict[str, Any], headers: dict[str, str]
    ) -> models.Dem:
        response = self._http.request(
            "PUT", self._project_path("dems", dem_id), json=body, headers=headers
        )
        return models.Dem.from_json(
            _json_of(response), etag=response.headers.get("ETag")
        )

    def delete_dem(self, dem_id: str) -> None:
        """Delete a DEM nobody links to. Needs ``operator``."""
        self._http.request("DELETE", self._project_path("dems", dem_id))

    def create_draft(
        self,
        radargram_id: str,
        steps: Iterable[str],
        *,
        medium_velocity: float | None = None,
    ) -> models.Draft:
        """Process a radargram's raw data on the server, to look at before
        it becomes the radargram. Needs ``operator``.

        Answers at once, with the draft queued; :meth:`wait_for_draft`
        follows it. The same request again answers the same draft. Refused
        with :class:`~ridal.client.errors.TooManyRequests` while the caller
        or the project has as many jobs as the server allows.

        What the recordings are (CRS, DEM, antenna overrides) is the
        source's: see :meth:`set_source`.
        """
        body: dict[str, Any] = {"steps": list(steps)}
        if medium_velocity is not None:
            body["medium_velocity"] = medium_velocity
        return models.Draft.from_json(
            self._http.json(
                "POST", self._project_path("sources", radargram_id, "drafts"), json=body
            )
        )

    def drafts(self) -> tuple[models.Draft, ...]:
        """The caller's drafts."""
        answer = self._http.json("GET", self._project_path("drafts"))
        return tuple(models.Draft.from_json(d) for d in answer["drafts"])

    def draft(self, job: int) -> models.Draft:
        """One of the caller's drafts, as far as it has got."""
        return models.Draft.from_json(
            self._http.json("GET", self._project_path("drafts", str(job)))
        )

    def wait_for_draft(
        self,
        job: int,
        *,
        on_update: Callable[[models.Draft], None] | None = None,
        interval: float = 1.0,
        timeout: float | None = None,
    ) -> models.Draft:
        """Follow a draft until it finishes, and return it as it ended.

        Parameters
        ----------
        job : int
        on_update : callable, optional
            Called with the draft each time it is looked at, so a script can
            report which step it is on (``draft.step``).
        interval : float, default 1
            Seconds between looks.
        timeout : float, optional
            Give up after this many seconds, with ``TimeoutError``. The draft
            carries on; :meth:`delete_draft` stops it.

        Returns
        -------
        Draft
            Finished: ``done``, or ``failed``, ``timed_out`` or
            ``cancelled``. Check ``state``; :meth:`process` raises instead.
        """
        return _follow(
            lambda: self.draft(job),
            lambda draft: draft.finished,
            on_update,
            interval,
            timeout,
            f"draft {job}",
        )

    def download_draft(
        self,
        job: int,
        destination: str | PathLike[str],
        *,
        progress: Progress | None = None,
    ) -> Path:
        """Download a finished draft's NetCDF file. Needs the ``all`` download
        scope."""
        return self._http.download(
            self._project_path("drafts", str(job), "download"),
            Path(destination),
            label=f"draft-{job}.nc",
            progress=progress,
        )

    def commit_draft(self, job: int) -> models.Staged | models.Added:
        """Make a finished draft the radargram.

        For a radargram already served, the draft is staged as a replacement
        and the answer is a :class:`~ridal.client.Staged`: read its report,
        then :meth:`commit_replacement` or :meth:`discard_replacement` it.
        For a new id the draft is added at once, and the answer is an
        :class:`~ridal.client.Added`. Refused
        (:class:`~ridal.client.errors.Conflict`, ``radargram_moved``) when
        someone has changed the radargram since the draft was asked for.
        """
        response = self._http.request(
            "POST", self._project_path("drafts", str(job), "commit")
        )
        body = _json_of(response)
        if response.status_code == 201:
            return models.Added.from_json(body)
        return models.Staged.from_json(body)

    def delete_draft(self, job: int) -> None:
        """Stop a draft, or throw a finished one away."""
        self._http.request("DELETE", self._project_path("drafts", str(job)))

    def process(
        self,
        radargram_id: str,
        recordings: Iterable[str | PathLike[str] | Iterable[str | PathLike[str]]],
        steps: Iterable[str],
        *,
        crs: str | None = None,
        dem: str | None = None,
        antenna_mhz: float | None = None,
        antenna_separation: float | None = None,
        medium_velocity: float | None = None,
        allow: Collection[models.Severity] = planning.DEFAULT_ALLOW,
        progress: Progress | None = None,
        on_update: Callable[[models.Draft], None] | None = None,
    ) -> models.Replaced | models.Added:
        """Process raw recordings into a radargram on the server, as the
        browser's "From raw data…" does: upload, set the source, draft, and
        apply.

        Parameters
        ----------
        radargram_id : str
        recordings : iterable
            The recordings in merge order. Each is a list of its files, or
            any one of its files (such as its ``.rd3``), in which case the
            files beside it with the same name and another extension are
            taken too.
        steps : iterable of str
            As ``ridal steps`` lists them.
        crs, dem : str, optional
            As for :meth:`set_source`.
        antenna_mhz, antenna_separation : float, optional
            As for :meth:`set_source`.
        medium_velocity : float, optional
            m/ns. The server's default is that of ice.
        allow : collection of Severity
            For a radargram already served: commit only if every
            interpretation on it would be at most this affected, as for
            :meth:`replace`. A refused replacement is discarded and
            :class:`~ridal.client.errors.ReplacementRefused` raised.
        progress : callable, optional
            For the uploads.
        on_update : callable, optional
            Called with the draft as it is processed.

        Raises
        ------
        ProcessingFailed
            The draft did not finish. It is left for :meth:`draft` to read.
        """
        stored = [
            [self.upload_raw(path, progress=progress) for path in files]
            for files in (_recording_files(r) for r in recordings)
        ]
        self.set_source(
            radargram_id,
            stored,
            crs=crs,
            dem=dem,
            antenna_mhz=antenna_mhz,
            antenna_separation=antenna_separation,
        )
        draft = self.create_draft(radargram_id, steps, medium_velocity=medium_velocity)
        draft = self.wait_for_draft(draft.job, on_update=on_update)
        if draft.state != "done":
            raise errors.ProcessingFailed(
                f"the draft of {radargram_id} ended {draft.state}"
                + (f": {draft.message}" if draft.message else ""),
                draft,
            )
        committed = self.commit_draft(draft.job)
        if isinstance(committed, models.Added):
            return committed
        reason = planning.acceptable(committed.report, allow)
        if reason is not None:
            self.discard_replacement(radargram_id, committed.token)
            raise errors.ReplacementRefused(reason, committed.report)
        return self.commit_replacement(radargram_id, committed.token)

    def profiles(self) -> tuple[models.Profile, ...]:
        """The project's processing profiles."""
        answer = self._http.json("GET", self._project_path("processing", "profiles"))
        return tuple(models.Profile.from_json(p) for p in answer["profiles"])

    def profile(self, profile_id: str) -> models.Profile:
        """One processing profile, every version of it, with its ``etag``."""
        response = self._http.request(
            "GET", self._project_path("processing", "profiles", profile_id)
        )
        return models.Profile.from_json(
            _json_of(response), etag=response.headers.get("ETag")
        )

    def save_profile(
        self,
        profile_id: str,
        *,
        name: str,
        steps: Iterable[str],
        medium_velocity: float,
        description: str = "",
        etag: str | None = None,
        overwrite: bool = False,
    ) -> models.Profile:
        """Create or change a processing profile. Needs ``operator``.

        A change to the steps or the velocity is a new version; a new name
        or description alone is not. ``etag`` and ``overwrite`` work as for
        :meth:`save_layers`: without an ``etag`` only a new profile is
        created, unless ``overwrite`` is set.
        """
        response = self._http.request(
            "PUT",
            self._project_path("processing", "profiles", profile_id),
            json={
                "name": name,
                "description": description,
                "steps": list(steps),
                "medium_velocity": medium_velocity,
            },
            headers=_preconditions(etag, overwrite),
        )
        return models.Profile.from_json(
            _json_of(response), etag=response.headers.get("ETag")
        )

    def delete_profile(self, profile_id: str) -> None:
        """Delete a processing profile no radargram is in. Needs ``operator``."""
        self._http.request(
            "DELETE", self._project_path("processing", "profiles", profile_id)
        )

    def assignments(self) -> dict[str, models.Assignment]:
        """Which processing profile each radargram is in, by radargram id."""
        answer = self._http.json("GET", self._project_path("processing", "assignments"))
        return {
            radargram_id: models.Assignment.from_json(assignment)
            for radargram_id, assignment in answer["radargrams"].items()
        }

    def assign(
        self, radargram_id: str, profile_id: str, *, pre_steps: Iterable[str] = ()
    ) -> models.Assignment:
        """Put a radargram in a processing profile, with steps of its own to
        run first (``subset`` and ``remove_traces`` only). Needs
        ``operator``, and raw data for the radargram."""
        return models.Assignment.from_json(
            self._http.json(
                "PUT",
                self._project_path("processing", "assignments", radargram_id),
                json={"profile": profile_id, "pre_steps": list(pre_steps)},
            )
        )

    def unassign(self, radargram_id: str) -> None:
        """Take a radargram out of its processing profile. Needs ``operator``."""
        self._http.request(
            "DELETE", self._project_path("processing", "assignments", radargram_id)
        )

    def processing_status(
        self, radargram_id: str | None = None
    ) -> tuple[models.RadargramStatus, ...]:
        """Where every radargram in a profile stands, or only ``radargram_id``."""
        params = {"radargram_id": radargram_id} if radargram_id is not None else None
        answer = self._http.json(
            "GET", self._project_path("processing", "status"), params=params
        )
        return tuple(models.RadargramStatus.from_json(r) for r in answer["radargrams"])

    def reprocess(
        self,
        profile_id: str,
        *,
        include_older: bool = False,
        radargram_ids: Iterable[str] | None = None,
    ) -> models.Batch:
        """Start a batch: process a profile's stale and not-processed
        radargrams at its current version, and apply each without review.
        Needs ``operator``.

        ``include_older`` takes those processed as the profile says by an
        older Ridal too; ``radargram_ids`` limits it to those of the
        profile's radargrams. Refused
        (:class:`~ridal.client.errors.Conflict`, ``nothing_to_reprocess``)
        when there is nothing to do. :meth:`wait_for_batch` follows it.
        """
        body: dict[str, Any] = {"include_older": include_older}
        if radargram_ids is not None:
            body["radargram_ids"] = list(radargram_ids)
        return models.Batch.from_json(
            self._http.json(
                "POST",
                self._project_path("processing", "profiles", profile_id, "reprocess"),
                json=body,
            )
        )

    def batches(self) -> tuple[models.Batch, ...]:
        """The 50 most recent batches, newest first. Needs ``operator``."""
        answer = self._http.json("GET", self._project_path("processing", "batches"))
        return tuple(models.Batch.from_json(b) for b in answer["batches"])

    def batch(self, batch_id: str) -> models.Batch:
        """One batch, item by item. Needs ``operator``."""
        return models.Batch.from_json(
            self._http.json(
                "GET", self._project_path("processing", "batches", batch_id)
            )
        )

    def wait_for_batch(
        self,
        batch_id: str,
        *,
        on_update: Callable[[models.Batch], None] | None = None,
        interval: float = 5.0,
        timeout: float | None = None,
    ) -> models.Batch:
        """Follow a batch until it has ended, as :meth:`wait_for_draft`
        follows a draft, and return it."""
        return _follow(
            lambda: self.batch(batch_id),
            lambda batch: batch.finished,
            on_update,
            interval,
            timeout,
            f"batch {batch_id}",
        )

    def _caller_name(self) -> str:
        user = self.me().user
        if user is None:
            raise ValueError(
                "Not signed in, so there are no own picks to export. Pass user=…."
            )
        return user


def _map(function: Callable[[_T], _R], items: Iterable[_T], workers: int) -> list[_R]:
    """``function`` over ``items`` on up to ``workers`` threads, in order.

    One ``httpx.Client`` is shared between them, which httpx supports. As
    soon as one call raises, those not yet started are cancelled; those in
    flight finish, and then the first exception in the order of ``items``
    is raised.

    >>> _map(str.upper, ["a", "b"], workers=2)
    ['A', 'B']
    """
    if workers < 1:
        raise ValueError(f"workers must be at least 1, not {workers}")
    # Checked by each call before it starts, so a worker that has just seen
    # a failure cannot pick up the next item before it is cancelled.
    failed = threading.Event()

    def call(item: _T) -> _R:
        if failed.is_set():
            raise _NotStarted
        try:
            return function(item)
        except BaseException:
            failed.set()
            raise

    with concurrent.futures.ThreadPoolExecutor(max_workers=workers) as executor:
        futures = [executor.submit(call, item) for item in items]
        try:
            concurrent.futures.wait(
                futures, return_when=concurrent.futures.FIRST_EXCEPTION
            )
        finally:
            # Also on Ctrl-C, so only the calls already running finish.
            failed.set()
            for future in futures:
                future.cancel()
    for future in futures:
        if future.cancelled():
            continue
        error = future.exception()
        if error is not None and not isinstance(error, _NotStarted):
            raise error
    return [future.result() for future in futures]


class _NotStarted(Exception):
    """A call :func:`_map` skipped because an earlier one failed."""


def _preconditions(etag: str | None, overwrite: bool) -> dict[str, str]:
    """The conditional headers for a save.

    Versions are hashes of the contents, so saving exactly what is stored
    again is never a conflict.

    >>> _preconditions('"v1"', overwrite=False)
    {'If-Match': '"v1"'}
    >>> _preconditions(None, overwrite=False)
    {'If-None-Match': '*'}
    >>> _preconditions(None, overwrite=True)
    {}
    """
    if etag is not None:
        return {"If-Match": etag}
    if overwrite:
        return {}
    return {"If-None-Match": "*"}


def _as_document(path: str, response: Any) -> models.Document:
    """A versioned document from a response. The server sends an empty
    ``ETag`` for a document that does not exist yet, which is ``None`` here."""
    try:
        data = response.json()
    except ValueError as error:
        raise errors.ProtocolError(f"{path} did not answer with JSON") from error
    return models.Document(data=data, etag=response.headers.get("ETag") or None)


def _json_of(response: Any) -> Any:
    try:
        return response.json()
    except ValueError as error:
        raise errors.ProtocolError(
            f"{response.request.method} {response.request.url.path} did not answer with JSON"
        ) from error


def _follow(
    look: Callable[[], _T],
    done: Callable[[_T], bool],
    on_update: Callable[[_T], None] | None,
    interval: float,
    timeout: float | None,
    what: str,
) -> _T:
    """Look at something until ``done`` says it is, and return it."""
    deadline = None if timeout is None else time.monotonic() + timeout
    while True:
        current = look()
        if on_update is not None:
            on_update(current)
        if done(current):
            return current
        if deadline is not None and time.monotonic() >= deadline:
            raise TimeoutError(f"{what} had not finished after {timeout} s")
        time.sleep(interval)


def _recording_files(recording: Any) -> list[Path]:
    """A recording's files: those given, or, for one file, it and the files
    beside it with the same name and another extension.

    >>> import tempfile
    >>> with tempfile.TemporaryDirectory() as d:
    ...     for name in ("a.rad", "a.rd3", "a.cor", "b.rd3"):
    ...         _ = (Path(d) / name).write_bytes(b"")
    ...     [p.name for p in _recording_files(Path(d) / "a.rd3")]
    ['a.cor', 'a.rad', 'a.rd3']
    """
    if isinstance(recording, (str, PathLike)):
        path = Path(recording)
        return sorted(
            sibling
            for sibling in path.parent.iterdir()
            if sibling.is_file() and sibling.stem == path.stem
        )
    return [Path(p) for p in recording]
