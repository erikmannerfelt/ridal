"""Sorting local radargrams against a server before changing anything.

:meth:`ridal.client.Client.plan` reads each local file's identity with
:func:`ridal.info` and compares it with the server's catalog, so nothing is
uploaded to find out what would happen. Only a file that would replace a
radargram somebody has picked is asked about, through the server's replace
preflight, which answers from the file's axes alone.
Up to ``workers`` files are looked at at the same time.
:meth:`ridal.client.Client.apply` then acts on the plan one file at a time:
the server takes uploads one at a time anyway, and staged uploads never pile
up against the project's size limit.
"""

from collections.abc import Collection
from dataclasses import dataclass
from pathlib import Path
from typing import Final, Literal, TypeAlias

from ridal.client import models

Status: TypeAlias = Literal["new", "unchanged", "safe", "risky", "legacy"]
"""What a local file is to the server.

``new``
    No radargram with its id is served: it would be uploaded.
``unchanged``
    The served radargram has this revision id: nothing to do. The id comes
    from the radargram id and the processing date, not the contents, so a
    file edited in another tool without reprocessing looks unchanged too;
    reprocess it to give it a revision of its own.
``safe``
    It would replace a radargram nobody has interpreted.
``risky``
    It would replace a radargram with interpretations; ``report`` says what
    would happen to them.
``legacy``
    Processed by a Ridal too old to have radargram ids. Reprocess it.
"""

Action: TypeAlias = Literal["uploaded", "replaced", "skipped", "failed"]

#: The severities a replacement may reach without being asked for.
DEFAULT_ALLOW: Final[frozenset[models.Severity]] = frozenset({"current", "carried"})


@dataclass(frozen=True)
class Record:
    """One local file, and what it is to the server."""

    path: Path
    status: Status
    radargram_id: str | None
    """``None`` only for a ``legacy`` file."""
    revision_id: str | None
    served_revision_id: str | None
    """The revision the server has under this id, if any."""
    report: models.ConsequenceReport | None = None
    """For ``risky``: the server's preflight answer."""
    reason: str | None = None
    """Why, in words, for ``legacy``."""


@dataclass(frozen=True)
class Plan:
    """Every local file given to :meth:`~ridal.client.Client.plan`, sorted."""

    records: tuple[Record, ...]

    def with_status(self, *statuses: Status) -> tuple[Record, ...]:
        """The records with any of ``statuses``."""
        return tuple(record for record in self.records if record.status in statuses)

    def summary(self) -> dict[Status, int]:
        """How many files have each status."""
        counts: dict[Status, int] = {}
        for record in self.records:
            counts[record.status] = counts.get(record.status, 0) + 1
        return counts


@dataclass(frozen=True)
class Outcome:
    """What :meth:`~ridal.client.Client.apply` did with one record."""

    record: Record
    action: Action
    detail: str
    """What happened, or why not, in words."""
    added: models.Added | None = None
    replaced: models.Replaced | None = None
    report: models.ConsequenceReport | None = None
    """For a replacement: the report the server gave on the staged file,
    which is what the decision was made on."""


def acceptable(
    report: models.ConsequenceReport, allow: Collection[models.Severity]
) -> str | None:
    """Why a replacement should not go ahead, or ``None`` if it may.

    The server refuses the first two at commit anyway; checking them here
    means a refused file is discarded rather than left staged.

    >>> report = models.ConsequenceReport(
    ...     radargram_id="line-01", from_revision="a", to_revision="b",
    ...     revision_id_collision=False, shape=None, outgoing_axes_kept=True,
    ...     documents=(), worst="approximate", headline="They move.",
    ... )
    >>> acceptable(report, DEFAULT_ALLOW)
    "'approximate' is not allowed: They move."
    >>> acceptable(report, {"approximate"}) is None
    True
    """
    if report.revision_id_collision:
        return report.headline
    if report.documents and not report.outgoing_axes_kept:
        return report.headline
    if report.worst not in allow:
        return f"'{report.worst}' is not allowed: {report.headline}"
    return None
