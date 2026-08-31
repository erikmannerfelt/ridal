#!/usr/bin/env python3
"""Benchmark the ridal web server's render paths.

Produces the numbers the roadmap's "measure before optimising" phase
needs, so the decision about server-side multiresolution tiling can be
made from data rather than intuition. Deliberately measures through HTTP
rather than calling the renderer directly: what matters is what a browser
experiences, including the permit queue and the cache.

Run against an already-started server:

    ridal server start /path/to/catalog --port 8000 &
    python3 scripts/bench_server.py --base-url http://127.0.0.1:8000

Reports, per scenario, the cold (first, uncached) time and the warm
(cached) distribution, since those differ by orders of magnitude and an
average over both is meaningless.
"""

from __future__ import annotations

import argparse
import json
import statistics
import time
import urllib.error
import urllib.request
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass, field

DEFAULT_BASE_URL = "http://127.0.0.1:8000"
# Enough repeats for a median and a p90 to mean something, few enough
# that a cold run of a large catalog finishes in reasonable time.
WARM_REPEATS = 20


@dataclass
class Timing:
    """One scenario's measurements, in seconds."""

    label: str
    cold: float | None = None
    warm: list[float] = field(default_factory=list)
    bytes_returned: int = 0
    errors: list[str] = field(default_factory=list)

    def row(self) -> str:
        cold = f"{self.cold * 1000:8.1f}" if self.cold is not None else "       -"
        if self.warm:
            median = statistics.median(self.warm) * 1000
            p90 = sorted(self.warm)[int(len(self.warm) * 0.9) - 1] * 1000
            warm = f"{median:8.1f} {p90:8.1f}"
        else:
            warm = "       -        -"
        size = f"{self.bytes_returned / 1024:8.1f}" if self.bytes_returned else "       -"
        note = f"  !! {len(self.errors)} errors" if self.errors else ""
        return f"{self.label:<44} {cold} {warm} {size}{note}"


def fetch(url: str) -> tuple[float, int, str | None]:
    """Return (elapsed_seconds, byte_count, error_or_None)."""
    started = time.perf_counter()
    try:
        with urllib.request.urlopen(url, timeout=300) as response:
            body = response.read()
        return time.perf_counter() - started, len(body), None
    except urllib.error.HTTPError as exc:
        # The server answers failures with a structured envelope; surface
        # its message rather than just the status.
        try:
            detail = json.loads(exc.read()).get("error", {}).get("message", "")
        except Exception:
            detail = ""
        return time.perf_counter() - started, 0, f"HTTP {exc.code} {detail}".strip()
    except Exception as exc:  # noqa: BLE001 - report anything, keep going
        return time.perf_counter() - started, 0, str(exc)


def measure(label: str, url: str, repeats: int = WARM_REPEATS) -> Timing:
    timing = Timing(label=label)
    elapsed, size, error = fetch(url)
    timing.cold = elapsed
    timing.bytes_returned = size
    if error:
        timing.errors.append(error)
    for _ in range(repeats):
        elapsed, _, error = fetch(url)
        if error:
            timing.errors.append(error)
        else:
            timing.warm.append(elapsed)
    return timing


def measure_parallel(label: str, urls: list[str], concurrency: int) -> Timing:
    """Wall time for a whole batch issued `concurrency`-wide.

    Models the index page, where a browser opens several connections at
    once -- the case the render permit semaphore actually governs.
    """
    timing = Timing(label=label)
    started = time.perf_counter()
    with ThreadPoolExecutor(max_workers=concurrency) as pool:
        results = list(pool.map(fetch, urls))
    timing.cold = time.perf_counter() - started
    for _, size, error in results:
        timing.bytes_returned += size
        if error:
            timing.errors.append(error)
    return timing


def get_json(url: str):
    with urllib.request.urlopen(url, timeout=60) as response:
        return json.loads(response.read())


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base-url", default=DEFAULT_BASE_URL)
    parser.add_argument(
        "--repeats",
        type=int,
        default=WARM_REPEATS,
        help="warm-cache repeats per scenario",
    )
    args = parser.parse_args()
    base = args.base_url.rstrip("/")

    datasets = get_json(f"{base}/api/v1/datasets")["entries"]
    profiles = get_json(f"{base}/api/v1/profiles")
    if not datasets:
        raise SystemExit("catalog is empty; nothing to benchmark")

    print(f"catalog: {len(datasets)} radargram(s), {len(profiles)} profile(s)")
    for entry in datasets:
        rows, cols = entry["shape"]
        print(f"  {entry['radargram_id']:<36} {cols} traces x {rows} samples")
    print()
    print(f"{'scenario':<44} {'cold ms':>8} {'warm ms':>8} {'p90 ms':>8} {'KB':>8}")
    print("-" * 82)

    results: list[Timing] = []

    # Page loads: the HTML itself, which does no rendering. Establishes
    # the floor that everything else is measured against.
    results.append(measure(f"{base}/ (index HTML)", f"{base}/", args.repeats))
    first = datasets[0]["radargram_id"]
    results.append(
        measure("/view/<id> (viewer HTML)", f"{base}/view/{first}", args.repeats)
    )

    # Overviews: the index page's per-entry thumbnails, and the single
    # most expensive request type since the input is the whole radargram.
    for entry in datasets:
        rid = entry["radargram_id"]
        results.append(
            measure(
                f"overview default  {rid[:24]}",
                f"{base}/api/v1/datasets/{rid}/views/standard/overview?profile=default",
                args.repeats,
            )
        )

    # One chunk, the viewer's unit of work. Compared against an overview
    # of the same radargram, this is the ratio that decides whether
    # multiresolution tiling would pay for itself.
    for entry in datasets:
        rid = entry["radargram_id"]
        results.append(
            measure(
                f"chunk 0,0 default {rid[:24]}",
                f"{base}/api/v1/datasets/{rid}/views/standard/chunks/default/0/0",
                args.repeats,
            )
        )

    # Track and axes: the JSON the viewer fetches on load. Cheap, but
    # they gate the overview map and the cursor readout.
    results.append(
        measure("track JSON", f"{base}/api/v1/datasets/{first}/track", args.repeats)
    )
    results.append(
        measure("axes JSON", f"{base}/api/v1/datasets/{first}/axes", args.repeats)
    )

    print("\n".join(r.row() for r in results))
    print()

    # Index first paint, modelled: every thumbnail at once, at the
    # browser's per-origin connection limit. Cold (nothing cached) is the
    # number a user actually waits for on first visit.
    thumb_urls = [
        f"{base}/api/v1/datasets/{e['radargram_id']}/views/standard/overview?profile=default"
        for e in datasets
    ]
    for concurrency in (1, 6):
        label = f"index thumbnails x{len(thumb_urls)}, {concurrency}-way (warm)"
        batch = measure_parallel(label, thumb_urls, concurrency)
        print(f"{batch.label:<44} {batch.cold * 1000:8.1f} ms total")

    errors = [e for r in results for e in r.errors]
    if errors:
        print(f"\n{len(errors)} error(s):")
        for message in sorted(set(errors)):
            print(f"  {message}")


if __name__ == "__main__":
    main()
