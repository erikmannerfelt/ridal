"""Progress reporting for long transfers, without a dependency on tqdm."""

import threading
from collections.abc import Callable
from dataclasses import dataclass
from typing import Any, Literal, TypeAlias


@dataclass(frozen=True)
class ProgressEvent:
    """How far one transfer has got.

    Attributes
    ----------
    stage : {"upload", "download"}
        Which way the bytes are going.
    label : str
        What is being transferred, usually a radargram id or a file name.
    done : int
        Bytes transferred so far.
    total : int or None
        The size in bytes, when it is known.
    """

    stage: Literal["upload", "download"]
    label: str
    done: int
    total: int | None


Progress: TypeAlias = Callable[[ProgressEvent], None]
"""A callback that receives a :class:`ProgressEvent` as a transfer advances.

:meth:`~ridal.client.Client.download_radargrams` calls it from several
threads at once, one per file in flight.
"""


def tqdm_progress(**options: Any) -> Progress:
    """A :data:`Progress` callback that draws a tqdm bar per transfer.

    Transfers running at the same time each get a bar of their own.

    Parameters
    ----------
    **options
        Passed to each ``tqdm.tqdm`` bar, such as ``leave=False``.

    Raises
    ------
    ImportError
        If tqdm is not installed.
    """
    try:
        import tqdm
    except ImportError as error:
        raise ImportError(
            "tqdm_progress() needs tqdm, which is not installed: pip install tqdm"
        ) from error

    bars: dict[tuple[str, str], Any] = {}
    # Concurrent downloads report from their own threads.
    lock = threading.Lock()

    def report(event: ProgressEvent) -> None:
        with lock:
            _advance(event)

    def _advance(event: ProgressEvent) -> None:
        key = (event.stage, event.label)
        bar = bars.get(key)
        if bar is None:
            bar = tqdm.tqdm(
                total=event.total,
                desc=f"{event.stage} {event.label}",
                unit="B",
                unit_scale=True,
                **options,
            )
            bars[key] = bar
        bar.update(event.done - bar.n)
        if event.total is not None and event.done >= event.total:
            bar.close()
            del bars[key]

    return report
