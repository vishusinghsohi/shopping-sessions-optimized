
#!/usr/bin/env python3
"""Compare Rust diagnostic CSV reports with official fixtures."""

import argparse
from collections import Counter
from pathlib import Path
import subprocess
import sys
import tempfile

COLUMNS = {
    "user_daily": (
        "user_id", "day", "sessions", "events",
        "revenue_cents", "top_category"
    ),
    "campaign_daily": (
        "campaign_id", "channel", "day", "sessions",
        "purchasing_sessions", "revenue_cents",
        "assisted_sessions"
    ),
}

INTEGER_FIELDS = {
    "sessions",
    "events",
    "revenue_cents",
    "purchasing_sessions",
    "assisted_sessions",
}

def parse_field(raw):
    # Quoted and unquoted empty fields represent null.
    # Embedded commas, quotes and newlines are not allowed.
    if raw.startswith(b'"') and raw.endswith(b'"') and len(raw) >= 2:
        raw = raw[1:-1]

    if b'"' in raw:
        raise ValueError("Invalid CSV quoting")

    return None if raw == b"" else raw.decode("utf-8")

def load_csv(path, table):
    if not path.is_file():
        raise FileNotFoundError(f"Missing file: {path}")

    # Split at LF only. CR is ordinary field data.
    lines = path.read_bytes().split(b"\n")

    if lines and lines[-1] == b"":
        lines.pop()

    if not lines:
        raise ValueError(f"Empty file: {path}")

    headers = tuple(
        parse_field(field) for field in lines[0].split(b",")
    )

    if headers != COLUMNS[table]:
        raise ValueError(
            f"{path}: unexpected columns {headers}"
        )

    rows = []

    for lineno, line in enumerate(lines[1:], start=2):
        fields = line.split(b",")

        if len(fields) != len(headers):
            raise ValueError(
                f"{path}:{lineno}: expected {len(headers)} fields"
            )

        values = [parse_field(field) for field in fields]

        for i, field in enumerate(headers):
            if field in INTEGER_FIELDS:
                values[i] = int(values[i])

        rows.append(tuple(values))

    return rows

def compare_table(expected, actual, table):
    exp = load_csv(expected, table)
    got = load_csv(actual, table)

    expected_counts = Counter(exp)
    actual_counts = Counter(got)

    if expected_counts == actual_counts:
        return True, f"{table}: {len(got)} rows match"

    missing = list(
        (expected_counts - actual_counts).elements()
    )[:3]

    extra = list(
        (actual_counts - expected_counts).elements()
    )[:3]

    return False, (
        f"{table}: expected {len(exp)} rows, got {len(got)}\n"
        f"    Missing examples: {missing}\n"
        f"    Extra examples:   {extra}"
    )

def run_fixture(binary, fixture):
    data = fixture / "data"

    if not data.is_dir():
        data = fixture / "input"

    if not data.is_dir():
        return False, "No input/data folder"

    with tempfile.TemporaryDirectory(
        prefix="shopping_fixture_"
    ) as tmp:

        working = Path(tmp)

        try:
            process = subprocess.run(
                [str(binary), str(data.resolve())],
                cwd=working,
                capture_output=True,
                text=True,
                timeout=90,
            )
        except subprocess.TimeoutExpired:
            return False, "Rust engine timed out"

        if process.returncode != 0:
            return False, (
                f"Rust exit code {process.returncode}\n"
                f"{process.stderr[-1200:]}\n"
                f"{process.stdout[-1200:]}"
            )

        results = []
        all_good = True

        for table in COLUMNS:
            expected = (
                fixture / "expected" / f"{table}.csv"
            )

            actual = (
                working / "out" / f"{table}.csv"
            )

            try:
                good, detail = compare_table(
                    expected, actual, table
                )
            except (
                OSError,
                ValueError,
                UnicodeError,
            ) as error:
                good = False
                detail = str(error)

            all_good = all_good and good
            results.append(detail)

        return all_good, "\n  ".join(results)

def main():
    parser = argparse.ArgumentParser()

    parser.add_argument(
        "--case",
        help="Run one fixture, for example A3"
    )

    parser.add_argument(
        "--no-build",
        action="store_true"
    )

    args = parser.parse_args()

    project = Path(__file__).resolve().parent.parent

    fixtures = project / "fixtures"

    manifest = (
        project / "rust_engine" / "Cargo.toml"
    )

    executable = (
        "rust_engine.exe"
        if sys.platform == "win32"
        else "rust_engine"
    )

    binary = (
        project / "rust_engine"
        / "target" / "release" / executable
    )

    if not fixtures.is_dir():
        sys.exit(
            f"Cannot find fixtures folder: {fixtures}"
        )

    if not args.no_build:
        print(
            "Building optimized Rust engine...",
            flush=True
        )

        subprocess.run(
            [
                "cargo",
                "build",
                "--release",
                "--manifest-path",
                str(manifest),
            ],
            cwd=project,
            check=True,
        )

    if not binary.is_file():
        sys.exit(
            f"Cannot find compiled executable: {binary}"
        )

    if args.case:
        cases = [fixtures / args.case]

        if not cases[0].is_dir():
            sys.exit(
                f"Fixture not found: {cases[0]}"
            )
    else:
        cases = sorted(
            folder
            for folder in fixtures.iterdir()
            if folder.is_dir()
            and (folder / "expected").is_dir()
        )

    failed = []

    for fixture in cases:
        good, detail = run_fixture(
            binary, fixture
        )

        status = "PASS" if good else "FAIL"

        print(
            f"{status} {fixture.name}: {detail}",
            flush=True
        )

        if not good:
            failed.append(fixture.name)

    print(
        f"\n{len(cases) - len(failed)}/{len(cases)} "
        "fixtures passed"
    )

    if failed:
        print(
            "Failed:",
            ", ".join(failed)
        )
        return 1

    return 0

if __name__ == "__main__":
    sys.exit(main())