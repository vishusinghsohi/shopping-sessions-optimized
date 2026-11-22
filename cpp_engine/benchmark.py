"""Pair competition-container runs against identical, staged Linux volumes."""

import argparse
import json
import re
import statistics
import subprocess
import sys
import time
import uuid
from pathlib import Path

import polars as pl

ROOT = Path(__file__).resolve().parents[1]
FLAGS = [
    "--rm", "--platform", "linux/amd64", "--network", "none", "--read-only",
    "--tmpfs", "/tmp:rw,size=64m,mode=1777", "--cap-drop", "ALL",
    "--security-opt", "no-new-privileges", "--pids-limit", "4096",
    "--cpus", "8", "--memory", "24g", "--memory-swap", "24g",
    "--user", "1000:1000", "-e", "NCPU=8", "-e", "MEM_GB=24",
]
SCHEMAS = {
    "user_daily": {
        "user_id": pl.String, "day": pl.Date, "sessions": pl.Int64, "events": pl.Int64,
        "revenue_cents": pl.Int64, "top_category": pl.String,
    },
    "campaign_daily": {
        "campaign_id": pl.String, "channel": pl.String, "day": pl.Date, "sessions": pl.Int64,
        "purchasing_sessions": pl.Int64, "revenue_cents": pl.Int64, "assisted_sessions": pl.Int64,
    },
}
EVICT = r"find /data -type f -exec dd if={} iflag=nocache count=0 status=none \;"


def command(*args):
    result = subprocess.run(args, capture_output=True, encoding="utf-8", errors="replace")
    if result.returncode:
        print(result.stdout, result.stderr, file=sys.stderr, flush=True)
        result.check_returncode()
    return result.stdout


def mount(kind, source, target, readonly=False):
    suffix = ",readonly" if readonly else ""
    if kind == "volume":
        suffix += ",volume-nocopy"
    return ["--mount", f"type={kind},source={source},target=/{target}{suffix}"]


def metrics(log, wall, output):
    result = {"docker_wall_seconds": wall}
    for name, value in re.findall(r"(?m)^\[(?:INNER|MEASURE)\] (\w+) ([\d.]+)$", log):
        result[name] = float(value)
    for name, expression in {
        "engine_seconds": r"(?:Rust|C\+\+) processing:\s+([\d.]+)",
        "engine_rss_mib": r"(?:Rust|C\+\+) peak RSS:\s+([\d.]+)",
        "csv_output_bytes": r"CSV output bytes: (\d+)",
        "input_bytes": r"Input bytes: (\d+)",
        "python_pipeline_seconds": r"Python pipeline total:\s+([\d.]+)",
    }.items():
        match = re.search(expression, log)
        if match:
            result[name] = float(match[1])
    result["parquet_output_bytes"] = sum(p.stat().st_size for p in output.glob("*/*.parquet"))
    for report in ("user_daily", "campaign_daily"):
        match = re.search(rf"(?m)^{report}: ([\d.]+) s", log)
        if match:
            result[report + "_export_seconds"] = float(match[1])
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--data", required=True, type=Path)
    parser.add_argument("--answer", type=Path, help="Published development answer or verified output")
    parser.add_argument("--artifacts", required=True, type=Path)
    parser.add_argument("--trials", type=int, default=3)
    parser.add_argument(
        "--evict", action="store_true",
        help="Request Linux page-cache eviction for staged input before each run",
    )
    parser.add_argument(
        "--profile-python-runtime", action="store_true",
        help="Use the in-container profiler for images with Python and /app/run.sh",
    )
    parser.add_argument("images", nargs="+", help="LABEL=IMAGE in baseline-to-candidate order")
    args = parser.parse_args()
    if args.trials < 1:
        parser.error("--trials must be positive")
    images = dict(item.split("=", 1) for item in args.images)
    if len(images) != len(args.images) or any(not re.fullmatch(r"[a-zA-Z0-9_-]+", label) for label in images):
        parser.error("use distinct alphanumeric image labels")
    data = args.data.resolve(strict=True)
    input_bytes = sum(file.stat().st_size for file in data.rglob("*") if file.is_file())
    uid = uuid.uuid4().hex[:12]
    output = (args.artifacts / uid).resolve()
    output.mkdir(parents=True)
    volumes = {}
    baseline = next(iter(images))
    print(f"Artifacts: {output}", flush=True)

    def volume(name, readonly=False):
        return mount("volume", volumes[name], name, readonly)

    def helper(mounts, script):
        command("docker", "run", "--rm", "--platform", "linux/amd64", "--network", "none", "--read-only",
                *mounts, "--entrypoint", "/bin/sh", images[baseline], "-ec", script)

    def trial(label, turn):
        directory = output / f"{label}-{turn}"
        directory.mkdir()
        if args.evict:
            helper(volume("data", True), EVICT)
        runtime = [images[label]]
        if args.profile_python_runtime:
            runtime = [
                *mount("bind", Path(__file__).with_name("profile_container.py").resolve(), "profile.py", True),
                "--entrypoint", "python3", images[label], "/profile.py",
            ]
        start = time.perf_counter()
        log = command(
            "docker", "run", *FLAGS, *volume("data", True), *volume("scratch"), *volume("output"),
            *runtime,
        )
        wall = time.perf_counter() - start
        (directory / "profile.log").write_text(log, encoding="utf-8")
        helper([*volume("output"), *volume("scratch"), *mount("bind", directory, "host")],
               "cp -R /output/. /host/ && rm -rf /output/user_daily /output/campaign_daily /scratch/out /scratch/duckdb_tmp")
        result = metrics(log, wall, directory)
        print(f"{label} {turn} {wall:.3f} s, engine {result['engine_seconds']:.3f} s, "
              f"RSS {result['engine_rss_mib']:.1f} MiB", flush=True)
        return result

    try:
        image_ids = {
            label: command("docker", "image", "inspect", "--format", "{{.Id}}", image).strip()
            for label, image in images.items()
        }
        staging_started = time.perf_counter()
        for name in ("data", "scratch", "output"):
            host_name = f"shopping-cpp23-{uid}-{name}"
            command("docker", "volume", "create", host_name)
            volumes[name] = host_name
        helper([*mount("bind", data, "source", True), *volume("data"), *volume("scratch"), *volume("output")],
               "cp -R /source/. /data/ && chown 1000:1000 /scratch /output")
        staging = time.perf_counter() - staging_started
        print(f"Input {input_bytes:,} bytes, staging {staging:.3f} s", flush=True)
        labels = list(images)
        samples = {label: [] for label in labels}
        for label in labels:
            trial(label, "warmup")
        for turn in range(args.trials):
            for label in (labels if turn % 2 == 0 else reversed(labels)):
                samples[label].append(trial(label, turn))
        expected = args.answer.resolve(strict=True) if args.answer else output / f"{baseline}-0"
        for label in labels:
            for turn in range(args.trials):
                trial_output = output / f"{label}-{turn}"
                for table, expected_schema in SCHEMAS.items():
                    for file in (trial_output / table).glob("*.parquet"):
                        if dict(pl.read_parquet_schema(file)) != expected_schema:
                            raise ValueError(f"wrong Parquet schema in {file}")
                command(sys.executable, "-B", str(ROOT / "scripts/compare.py"),
                        str(trial_output), str(expected))
            wall = [sample["docker_wall_seconds"] for sample in samples[label]]
            print(f"{label} median {statistics.median(wall):.3f} s, range {min(wall):.3f}–{max(wall):.3f} s", flush=True)
        head = subprocess.run(
            ["git", "-C", str(ROOT), "rev-parse", "HEAD"],
            stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, encoding="utf-8", check=False,
        )
        summary = {
            "dataset": str(data), "input_bytes": input_bytes, "staging_seconds": staging,
            "worktree_base": head.stdout.strip() if head.returncode == 0 else "uncommitted",
            "cache": "eviction requested" if args.evict else "warmed",
            "flags": FLAGS, "images": images, "image_ids": image_ids,
            "comparison": "MATCH", "samples": samples,
        }
        (output / "summary.json").write_text(json.dumps(summary, indent=2) + "\n", encoding="utf-8")
        print("All timed reports MATCH.", flush=True)
    finally:
        for name in volumes.values():
            command("docker", "volume", "rm", name)


if __name__ == "__main__":
    main()
