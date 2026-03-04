#!/usr/bin/env python3
import sys
import re
from pathlib import Path

HEADING_RE = re.compile(r'^(#+)\s+')

def check_file(path: Path) -> int:
    errors = []
    text = path.read_text(encoding="utf-8", errors="replace")
    lines = text.splitlines()

    for i, line in enumerate(lines, start=1):
        m = HEADING_RE.match(line)
        if not m:
            continue
        hashes = m.group(1)

        # Allow only "## " headings
        if hashes != "##":
            errors.append(
                f"{path}:{i}: only '##' headings are allowed "
                f"(found '{hashes} ')"
            )

    if errors:
        print("\n".join(errors), file=sys.stderr)
        return 1
    return 0

def main(argv: list[str]) -> int:
    status = 0
    for arg in argv[1:]:
        path = Path(arg)
        if not path.exists():
            continue
        status |= check_file(path)
    return status

if __name__ == "__main__":
    raise SystemExit(main(sys.argv))

