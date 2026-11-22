
#!/usr/bin/env python3
"""
Shopping Sessions — optimized Parquet output.

The selected engine performs all competition business logic.

For ordinary diagnostic CSV reports:
    Engine CSV -> DuckDB -> Parquet

For reports containing special CSV characters:
    Engine CSV -> safe JSON conversion -> DuckDB -> Parquet

Both paths produce the required typed Parquet schemas.
"""

import json
import os
import resource
import subprocess
import time
from pathlib import Path

import duckdb

SCHEMAS = {
    "user_daily": [
        ("user_id", "VARCHAR"),
        ("day", "DATE"),
        ("sessions", "BIGINT"),
        ("events", "BIGINT"),
        ("revenue_cents", "BIGINT"),
        ("top_category", "VARCHAR"),
    ],
    "campaign_daily": [
        ("campaign_id", "VARCHAR"),
        ("channel", "VARCHAR"),
        ("day", "DATE"),
        ("sessions", "BIGINT"),
        ("purchasing_sessions", "BIGINT"),
        ("revenue_cents", "BIGINT"),
        ("assisted_sessions", "BIGINT"),
    ],
}

def sql_string(value):
    """Escape a string for use in a DuckDB SQL statement."""
    return "'" + str(value).replace("'", "''") + "'"

def sql_identifier(value):
    """Escape a SQL column identifier."""
    return '"' + value.replace('"', '""') + '"'

def typed_select(schema):
    """Select columns in the exact competition order."""
    return ", ".join(
        f"CAST({sql_identifier(name)} AS {kind}) "
        f"AS {sql_identifier(name)}"
        for name, kind in schema
    )

def inspect_diagnostic_csv(path, schema):
    """
    Check whether DuckDB's fast CSV reader is safe
    for this particular engine report.

    Direct CSV is used only if:
    - The header matches.
    - No carriage returns appear in data.
    - No double-quote characters appear in data.

    The report format never embeds commas or LF
    inside fields.

    Special-character cases use our existing,
    byte-preserving JSON conversion instead.
    """
    names = [name for name, _ in schema]

    expected = ",".join(names).encode("utf-8")

    with path.open("rb") as source:
        header = source.readline()

        if header.endswith(b"\n"):
            header = header[:-1]

        if header != expected:
            raise ValueError(
                f"Unexpected columns in {path}: {header!r}"
            )

        has_rows = False
        safe_for_direct_csv = True

        while True:
            block = source.read(4 * 1024 * 1024)

            if not block:
                break

            has_rows = True

            if b"\r" in block or b'"' in block:
                safe_for_direct_csv = False

    return has_rows, safe_for_direct_csv

def diagnostic_csv_to_jsonl(csv_path, json_path, schema):
    """
    Safe fallback for the competition's unusual
    characters and exact LF-only row boundaries.
    """
    names = [name for name, _ in schema]

    numeric_names = {
        name
        for name, kind in schema
        if kind == "BIGINT"
    }

    count = 0

    with csv_path.open("rb") as source, json_path.open(
        "w",
        encoding="utf-8",
        newline="\n",
    ) as destination:

        header = source.readline()

        if header.endswith(b"\n"):
            header = header[:-1]

        expected_header = ",".join(names).encode("utf-8")

        if header != expected_header:
            raise ValueError(
                f"Unexpected CSV header: {header!r}"
            )

        for raw in source:
            if raw.endswith(b"\n"):
                raw = raw[:-1]

            cells = raw.split(b",")

            if len(cells) != len(names):
                raise ValueError(
                    f"Malformed diagnostic CSV row: {csv_path}"
                )

            record = {}

            for (name, _), raw_field in zip(schema, cells):
                value = raw_field.decode("utf-8")

                if name in numeric_names:
                    record[name] = int(value)

                elif name == "top_category" and value == "":
                    record[name] = None

                else:
                    record[name] = value

            destination.write(
                json.dumps(
                    record,
                    ensure_ascii=True,
                    separators=(",", ":"),
                )
                + "\n"
            )

            count += 1

    return count

def copy_to_parquet(con, query, destination):
    """Execute an actual DuckDB Parquet export."""
    destination.parent.mkdir(
        parents=True,
        exist_ok=True,
    )

    con.execute(
        f"COPY ({query}) "
        f"TO {sql_string(destination)} "
        "(FORMAT PARQUET, COMPRESSION 'SNAPPY')"
    )

def empty_query(schema):
    columns = ", ".join(
        f"CAST(NULL AS {kind}) AS {sql_identifier(name)}"
        for name, kind in schema
    )

    return f"SELECT {columns} WHERE FALSE"

def fast_csv_query(csv_path, schema):
    """
    Explicitly read the safe subset of the engine's
    diagnostic CSV using DuckDB's native CSV reader.

    All columns are read as VARCHAR first to prevent
    unexpected numeric or date inference.
    """
    column_types = ", ".join(
        f"{sql_string(name)}: 'VARCHAR'"
        for name, _ in schema
    )

    return (
        f"SELECT {typed_select(schema)} "
        f"FROM read_csv("
        f"{sql_string(csv_path)}, "
        "auto_detect=false, "
        "header=true, "
        "delim=',', "
        "quote='', "
        "escape='', "
        "nullstr='', "
        f"columns={{{column_types}}})"
    )

def json_query(json_path, schema):
    column_types = ", ".join(
        f"{sql_string(name)}: "
        f"{sql_string('BIGINT' if kind == 'BIGINT' else 'VARCHAR')}"
        for name, kind in schema
    )

    return (
        f"SELECT {typed_select(schema)} "
        f"FROM read_json("
        f"{sql_string(json_path)}, "
        "format='newline_delimited', "
        f"columns={{{column_types}}})"
    )

def write_report(
    con,
    report_name,
    schema,
    reports_dir,
    scratch_dir,
    output_dir,
):
    csv_path = reports_dir / f"{report_name}.csv"

    destination = (
        output_dir / report_name / "part-0.parquet"
    )

    if not csv_path.is_file():
        raise FileNotFoundError(
            f"Engine did not create {csv_path}"
        )

    scan_start = time.perf_counter()

    has_rows, fast_safe = inspect_diagnostic_csv(
        csv_path,
        schema,
    )

    scan_seconds = time.perf_counter() - scan_start

    print(
        f"[PROFILE] {report_name} safety scan: "
        f"{scan_seconds:.3f} s",
        flush=True,
    )

    if not has_rows:
        start = time.perf_counter()

        copy_to_parquet(
            con,
            empty_query(schema),
            destination,
        )

        elapsed = time.perf_counter() - start

        print(
            f"[PROFILE] {report_name}: "
            f"empty Parquet written in {elapsed:.3f} s",
            flush=True,
        )

        csv_path.unlink(missing_ok=True)
        return "empty", scan_seconds + elapsed

    # -------------------------------------------------
    # FAST PATH: Native DuckDB CSV to Parquet
    # -------------------------------------------------

    if fast_safe:
        start = time.perf_counter()

        try:
            copy_to_parquet(
                con,
                fast_csv_query(csv_path, schema),
                destination,
            )

            elapsed = time.perf_counter() - start

            print(
                f"[PROFILE] {report_name}: "
                f"FAST CSV -> Parquet in {elapsed:.3f} s",
                flush=True,
            )

            csv_path.unlink(missing_ok=True)

            return "fast", scan_seconds + elapsed

        except duckdb.Error as error:
            # If direct CSV parsing is not supported
            # for this input, keep exact conversion.
            print(
                f"[PROFILE] {report_name}: "
                f"fast reader unavailable ({error}); "
                "using safe fallback",
                flush=True,
            )

            destination.unlink(missing_ok=True)

    # -------------------------------------------------
    # SAFE PATH: Byte-preserving CSV to JSON to Parquet
    # -------------------------------------------------

    json_path = scratch_dir / f"{report_name}.jsonl"

    start = time.perf_counter()

    row_count = diagnostic_csv_to_jsonl(
        csv_path,
        json_path,
        schema,
    )

    conversion_seconds = time.perf_counter() - start

    start = time.perf_counter()

    copy_to_parquet(
        con,
        json_query(json_path, schema),
        destination,
    )

    parquet_seconds = time.perf_counter() - start

    print(
        f"[PROFILE] {report_name}: "
        f"SAFE conversion for {row_count} rows",
        flush=True,
    )

    print(
        f"[PROFILE] {report_name}: "
        f"CSV -> JSON {conversion_seconds:.3f} s, "
        f"Parquet {parquet_seconds:.3f} s",
        flush=True,
    )

    json_path.unlink(missing_ok=True)
    csv_path.unlink(missing_ok=True)

    return (
        "safe",
        scan_seconds + conversion_seconds + parquet_seconds,
    )

def main():
    total_start = time.perf_counter()

    data_dir = Path(
        os.environ.get("DATA_DIR", "/data")
    ).resolve()

    output_dir = Path(
        os.environ.get("OUTPUT_DIR", "/output")
    ).resolve()

    scratch_dir = Path(
        os.environ.get("SCRATCH_DIR", "/scratch")
    ).resolve()

    ncpu = max(
        1,
        int(os.environ.get("NCPU", "8")),
    )

    mem_gb = max(
        1,
        int(os.environ.get("MEM_GB", "24")),
    )

    scratch_dir.mkdir(parents=True, exist_ok=True)
    output_dir.mkdir(parents=True, exist_ok=True)

    # -------------------------------------------------
    # PHASE 1: Engine processing
    # -------------------------------------------------

    engine = Path(os.environ.get("SESSION_ENGINE", "/app/rust_engine"))
    engine_label = "C++" if engine.name == "cpp_engine" else "Rust"
    print(f"\n[PROFILE] Starting {engine_label} engine", flush=True)

    engine_start = time.perf_counter()

    subprocess.run(
        [str(engine), str(data_dir)],
        cwd=scratch_dir,
        check=True,
    )

    engine_seconds = time.perf_counter() - engine_start

    engine_peak_mib = (
        resource.getrusage(
            resource.RUSAGE_CHILDREN
        ).ru_maxrss / 1024.0
    )

    print(
        f"[PROFILE] {engine_label} time: {engine_seconds:.3f} s",
        flush=True,
    )

    print(
        f"[PROFILE] {engine_label} peak RSS: {engine_peak_mib:.1f} MiB",
        flush=True,
    )

    # -------------------------------------------------
    # PHASE 2: DuckDB configuration
    # -------------------------------------------------

    con = duckdb.connect(database=":memory:")

    con.execute(f"SET threads = {ncpu}")

    con.execute(
        f"SET memory_limit = "
        f"'{max(1, int(mem_gb * 0.75))}GB'"
    )

    con.execute(
        "SET temp_directory = "
        + sql_string(scratch_dir / "duckdb_tmp")
    )

    con.execute(
        "SET preserve_insertion_order = false"
    )

    # -------------------------------------------------
    # PHASE 3: Output reports
    # -------------------------------------------------

    reports_dir = scratch_dir / "out"

    report_times = {}

    for name, schema in SCHEMAS.items():
        mode, seconds = write_report(
            con,
            name,
            schema,
            reports_dir,
            scratch_dir,
            output_dir,
        )

        report_times[name] = (
            mode,
            seconds,
        )

    con.close()

    total_seconds = time.perf_counter() - total_start

    # -------------------------------------------------
    # PERFORMANCE SUMMARY
    # -------------------------------------------------

    print("\n====================================")
    print("       OPTIMIZED PIPELINE PROFILE")
    print("====================================")
    print(f"{engine_label} processing: {engine_seconds:.3f} s")
    print(f"{engine_label} peak RSS:   {engine_peak_mib:.1f} MiB")

    for name, (mode, seconds) in report_times.items():
        print(
            f"{name}: {seconds:.3f} s ({mode} path)"
        )

    print(f"Python pipeline total: {total_seconds:.3f} s")
    print("====================================")
    print("Both Parquet reports created successfully")

if __name__ == "__main__":
    main()
