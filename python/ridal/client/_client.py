"""The public :class:`Client`."""

import concurrent.futures
import os
import threading
from collections.abc import Callable, Collection, Iterable
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
            self._http.json(
                "POST", self._dataset_path(radargram_id, "replace", token), json={}
            )
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
    ) -> tuple[planning.Outcome, ...]:
        """Upload the new files of a plan and replace the changed ones.

        One file at a time: a replacement is staged, checked against
        ``allow`` on the server's own report for the uploaded file, and
        committed or discarded before the next begins. A file that fails is
        reported in its outcome and the rest continue.

        Unlike :meth:`plan`, this does not run files side by side. The
        server takes uploads one at a time, because measuring the room left
        and installing a file are one decision, so a second upload would
        only wait, and on a slow link wait long enough to time out.
        """
        outcomes = []
        for record in planned.records:
            if record.status in ("unchanged", "legacy"):
                detail = record.reason or "the server already has this revision"
                outcomes.append(planning.Outcome(record, "skipped", detail))
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
