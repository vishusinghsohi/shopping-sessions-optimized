
import argparse
import subprocess
import sys
import tempfile
from collections import Counter
from pathlib import Path

# Required competition report columns.
SCHEMAS = {
    "user_daily": [
        "user_id", "day", "sessions", "events",
        "revenue_cents", "top_category",
    ],
    "campaign_daily": [
        "campaign_id", "channel", "day",
        "sessions", "purchasing_sessions",
        "revenue_cents", "assisted_sessions",
    ],
}

INTEGER_COLUMNS = {
    "sessions", "events", "revenue_cents",
    "purchasing_sessions", "assisted_sessions",
}


def parse_field(raw):
    if len(raw) >= 2 and raw.startswith(b'"') and raw.endswith(b'"'):
        raw = raw[1:-1]

    if b'"' in raw:
        raise ValueError("Invalid quoting")

    if raw == b"":
        return None

    return raw.decode("utf-8")


def read_report(path, report):
    if not path.is_file():
        raise FileNotFoundError(f"Missing report: {path}")

    # Split on LF only. Preserve CR in field values.
    lines = path.read_bytes().split(b"\n")

    if lines and lines[-1] == b"":
        lines.pop()

    if not lines:
        raise ValueError(f"Empty report: {path}")

    header = [
        parse_field(value)
        for value in lines[0].split(b",")
    ]

    if header != SCHEMAS[report]:
        raise ValueError(
            f"Wrong columns in {path}: {header}"
        )

    rows = []

    for line in lines[1:]:
        fields = line.split(b",")

        if len(fields) != len(header):
            raise ValueError(
                f"Wrong number of columns in {path}"
            )

        values = [
            parse_field(value)
            for value in fields
        ]

        for index, column in enumerate(header):
            if column in INTEGER_COLUMNS:
                values[index] = int(values[index])

        rows.append(tuple(values))

    return rows


def test_fixture(binary, fixture):
    data = fixture / "input"

    if not data.is_dir():
        data = fixture / "data"

    if not data.is_dir():
        return False, "Input folder missing"

    with tempfile.TemporaryDirectory() as temp:
        working_dir = Path(temp)

        try:
            process = subprocess.run(
                [str(binary), str(data)],
                cwd=working_dir,
                capture_output=True,
                text=True,
                timeout=90,
            )
        except subprocess.TimeoutExpired:
            return False, "Execution timed out"

        if process.returncode != 0:
            return False, (
                f"Rust failed:\n{process.stderr[-1500:]}"
            )

        results = []
        passed = True

        for report in SCHEMAS:
            expected = (
                fixture / "expected" / f"{report}.csv"
            )
            actual = (
                working_dir / "out" / f"{report}.csv"
            )

            try:
                expected_rows = read_report(expected, report)
                actual_rows = read_report(actual, report)

                missing = (
                    Counter(expected_rows) - Counter(actual_rows)
                )
                extra = (
                    Counter(actual_rows) - Counter(expected_rows)
                )

                if missing or extra:
                    passed = False
                    results.append(
                        f"{report}: MISMATCH\n"
                        f"  Missing: {list(missing.elements())[:2]}\n"
                        f"  Extra: {list(extra.elements())[:2]}"
                    )
                else:
                    results.append(
                        f"{report}: {len(actual_rows)} rows match"
                    )

            except (OSError, ValueError, TypeError) as error:
                passed = False
                results.append(
                    f"{report}: {error}"
                )

        return passed, "\n  ".join(results)


def main():
    parser = argparse.ArgumentParser(
        description="Verify Rust output against official fixtures"
    )
    parser.add_argument("--case", default=None)
    args = parser.parse_args()

    project = Path(__file__).resolve().parent.parent
    fixtures = project / "fixtures"
    manifest = project / "rust_engine" / "Cargo.toml"

    filename = (
        "rust_engine.exe"
        if sys.platform == "win32"
        else "rust_engine"
    )

    binary = (
        project / "rust_engine"
        / "target" / "release" / filename
    )

    if not fixtures.is_dir():
        sys.exit(f"Fixtures folder missing: {fixtures}")

    print("Building optimized Rust engine...", flush=True)

    subprocess.run(
        [
            "cargo", "build", "--release",
            "--manifest-path", str(manifest),
        ],
        cwd=project,
        check=True,
    )

    if args.case:
        cases = [fixtures / args.case]
    else:
        cases = sorted(
            folder for folder in fixtures.iterdir()
            if folder.is_dir()
            and (folder / "expected").is_dir()
        )

    if not cases:
        sys.exit("No fixtures found")

    failed = []

    for fixture in cases:
        if not fixture.is_dir():
            sys.exit(f"Fixture does not exist: {fixture}")

        passed, details = test_fixture(binary, fixture)

        status = "PASS" if passed else "FAIL"

        print(f"{status} {fixture.name}")
        print(f"  {details}", flush=True)

        if not passed:
            failed.append(fixture.name)

    total = len(cases)
    succeeded = total - len(failed)

    print(f"\nFINAL RESULT: {succeeded}/{total} passed")

    if failed:
        print("Failed fixtures:", ", ".join(failed))
        return 1

    return 0


if __name__ == "__main__":
    sys.exit(main())
