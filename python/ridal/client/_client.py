"""The public :class:`Client`."""

import os
from os import PathLike
from pathlib import Path
from types import TracebackType
from typing import Any, Final, Literal, Self, TypeAlias

from ridal.client import _http, errors, models
from ridal.client.progress import Progress

#: Where the client looks for an API token when none is passed.
TOKEN_VARIABLE: Final = "RIDAL_TOKEN"

#: The project key ``ridal gui`` serves its one project under.
DEFAULT_PROJECT: Final = "default"

Spacing: TypeAlias = Literal["auto", "per-trace", "vertices"] | float


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

    def track(self, radargram_id: str) -> dict[str, Any]:
        """The radargram's track as GeoJSON, in WGS84, one feature per segment."""
        return self._http.json("GET", self._dataset_path(radargram_id, "track.geojson"))

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
        params: dict[str, str] = {"format": format}
        if spacing is not None:
            params["spacing"] = str(spacing)
        if crs is not None:
            params["crs"] = crs
        if derived:
            path = self._dataset_path(radargram_id, "derived", "level2")
        else:
            who = user if user is not None else self._caller_name()
            path = self._dataset_path(radargram_id, "interpretations", who, "level2")
        return self._http.download(
            path,
            Path(destination),
            params=params,
            label=f"{radargram_id}.{'csv' if format == 'csv' else 'geojson'}",
            progress=progress,
        )

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

    def _caller_name(self) -> str:
        user = self.me().user
        if user is None:
            raise ValueError(
                "Not signed in, so there are no own picks to export. Pass user=…."
            )
        return user
