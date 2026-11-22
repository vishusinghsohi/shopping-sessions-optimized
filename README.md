# Shopping Sessions

## Competition build

The default Docker target builds `cpp_engine/` with CMake,
runs CTest during the image build, maps read-only event and click partitions,
enables link-time optimization, and uses the typed DuckDB export. The `cpp` target reads partitions
into owned buffers. The `rust` target runs the tracked Rust fallback with the
same entrypoint and export. Run these commands from the repository root.

```sh
docker build --no-cache --platform linux/amd64 --target competition -t shopping-sessions:cpp23 .
docker build --platform linux/amd64 --target rust -t shopping-sessions:rust-fallback .
cargo test --locked --manifest-path rust_engine/Cargo.toml
```

For a standalone C++ build, install a C++23 compiler, CMake 3.20 or newer,
and the nlohmann JSON development package. The JSON library is used only
for `manifest.json`.

```sh
cmake -S cpp_engine -B scratch/cpp-build -DCMAKE_BUILD_TYPE=Release
cmake --build scratch/cpp-build --parallel
ctest --test-dir scratch/cpp-build --output-on-failure
```

The Windows fixture runner accepts the image in the `IMAGE` environment
variable. With Polars installed, run the fixtures and a paired public dev
benchmark from PowerShell:

```powershell
$env:IMAGE = 'shopping-sessions:cpp23'
python scripts/run_fixtures.py fixtures -- python scripts/docker_run_windows.py
python cpp_engine/benchmark.py --data path/to/dev --answer path/to/dev-answer --artifacts scratch/benchmarks --trials 3 rust=shopping-sessions:rust-fallback cpp=shopping-sessions:cpp23
```

The benchmark stages the input into a Linux-managed Docker volume, warms each
image once, and alternates measured runs. It records staging separately and
checks both Parquet tables after timing. Keep the logs and `summary.json` files
under `scratch/`. Generated competition inputs and answers belong outside Git.
The optional `--evict` flag requests Linux page-cache eviction of the staged
files before each run. Other storage caches may still be warm.

Keep the downloaded starter archives, public development answer, generator
build, and generated datasets outside Git. The checked-in public fixture
suite comes from the DuckDB starter. Git preserves every fixture byte
because a carriage return is data in the competition CSV dialect.

```sh
python scripts/verify_fixture_bytes.py path/to/starter-duckdb.zip
```

The C++ loader scans contiguous ranges of the event files in parallel and
routes each ID to one shard. Each shard visits its ranges in file-and-line
order, keeps the earliest-ingest winner per ID, and then validates that
winner. The click loader keeps the first occurrence of each click ID in a
contiguous lookup table. Both loaders compare full ID bytes. Input buffers or
read-only mappings own every borrowed view until the engine materializes the
needed values. The current engine keeps winners and sorted events in memory.

The [official rules](https://www.joinbuilders.org/buildathon/tracks/shopping-sessions/files/rules.md)
define the CSV dialect, output schemas, and all attribution rules.

### Local benchmark report

Parallel ingestion reduced median Docker wall time from **16.703 s to
13.629 s** on 6 million generated events and 1.5 million clicks. Three timed
runs per build alternated on the same Linux-managed volumes after warmup.
Staging took 18.075 s and was measured separately. Both output tables and
their schemas matched across all runs.

| 6M workload measurement | Previous release | Parallel ingestion |
|---|---:|---:|
| Docker wall time | 16.703 s | 13.629 s |
| C++ engine time | 14.174 s | 10.864 s |
| Event routing | 3.219 s | 0.494 s |

Across two public dev batches with seven timed runs per build, the median
Docker wall time went from 2.368 s to 2.237 s. The engine median went from
1.193 s to 0.898 s, and every output matched the published answer. The base
revision for these comparisons was `2fe5543`.

#### Earlier measurements

The previous default was selected on a paired, Linux-volume benchmark with
6 million generated events and 1.5 million clicks. All reports matched.
Input staging took 21.30 s and was measured separately.

| C++ build | Median wall time | Timed runs |
|---|---:|---|
| Previously verified build | 15.97 s | 11.30 s, 20.64 s |
| Packed field offsets | 14.39 s | 11.23 s, 17.55 s |
| Link-time optimization, selected | 12.94 s | 12.94 s, 12.95 s |

The two runs per build show considerable host variability. The selected
build's result is a local benchmark. The last reported official runtime
from the earlier repository was 94.0685 s.

On the 1M public dev set, the release and Rust fallback medians were 1.595 s
and 1.865 s across two runs each. Both matched the published expected tables
exactly with the required Parquet schemas and passed all 36 official Docker
fixtures. CTest passed for the C++
build, and all 42 Rust unit tests passed. The staged fixture files matched
all 290 files in the published starter archive.

The comparison below predates this repository. The paired Rust comparator was the locally
optimized `shopping-sessions:competition-shared-clicks` image. Each row below
uses a separate paired Linux Docker experiment on the same staged input.
Wall time includes the Docker invocation and excludes staging and output
comparison. RSS is the engine process peak.

| Workload | Input bytes | Timed runs each | Rust median / RSS | Mapped C++ median / RSS | Reports |
|---|---:|---:|---:|---:|---|
| Public dev, 1M events | 96,210,428 | 3 | 2.06 s / 217 MiB | 2.67 s / 163 MiB | MATCH |
| Seed 6502, 6M events and 1.5M clicks | 568,364,106 | 2 | 19.08 s / 964 MiB | 17.14 s / 986 MiB | MATCH |
| Seed 6502, 10M events and 2.5M clicks | 946,354,118 | 2 | 78.08 s / 1,544 MiB | 66.47 s / 1,667 MiB | MATCH |

The 10M run took 57.55 s to stage. Rust ranged from 65.40 to 90.76 s and
C++ ranged from 61.49 to 71.44 s. The C++ engine wrote 34,474,093 diagnostic
CSV bytes and about 8.1 MB of Parquet. Its largest measured stages were shard
processing at 16.70 s, identity merge at 13.83 s, and input routing at 9.90 s.
The typed DuckDB export took about 1.28 s. The mapped engine's process RSS
includes file-backed input pages.

Sharding and the compact winner table reduced the public dev C++ engine time
from 2.19 to 0.98 s in the same comparison. Mapped C++ ran at 17.14 s versus
24.63 s with buffered input on 6M. In two separate dev trials with advisory
Linux input-cache eviction, mapped and buffered medians were 2.73 and 3.46 s.
The parallel identity-lookup experiment reduced merge time but raised 6M wall
time from 18.20 to 23.82 s. The default uses the earlier mapped engine.

The local Docker VM had 3.68 GiB. The 30M and 60M workloads require further
memory and runtime measurements on a larger Linux machine. These local
measurements give no official score for the C++ branch.

## What to do first

- Read the official rules before changing the engine or export.
- Use `cpp_engine/` for C++ processing and `pipeline/pipeline.py` for typed export.
- Build the image and test against all 36 edge-case fixtures and the 1M-event dev set before submitting.

## Run and test locally

You need Docker with buildx, and Python 3.10+ with `polars` for the test scripts. Install it with `pip install polars`.

Build the image and run all 36 fixtures:

```sh
make build        # docker buildx build --platform linux/amd64 --load -t shopping-sessions:latest .
make fixtures     # runs the image on all 36 fixtures with scorer flags, diffs each against expected/
```

Build, run, and check the 1M-event dev set. `make dev` needs `generate` on your PATH: build it from the track page's generator.zip (Rust 1.85 or newer; its README has the two commands).

```sh
make dev                                # generate --seed 1 --events 1000000 --clicks 250000 --output dev
make run-dev                            # runs your image on dev/ with scorer flags and times it
make check-dev                          # exact diff against dev-answer/, the track page's dev-answer.zip unzipped here
```

If your laptop cannot spare 8 cores and 24 GB, run with lower limits using `make run-dev CPUS=4 MEM=8g`. Your local times will not match the scorer anyway; only the board's times count.

- **x86 Linux and Windows machines.** Build and run the amd64 image natively.
- **Apple-silicon Macs.** Emulating amd64 is slow. For fast local loops, build a native image with `make build run-dev PLATFORM=linux/arm64`. The Dockerfile pins no platform, so it builds either way. Check the amd64 image before you submit.

To iterate without Docker:

```sh
pip install duckdb==1.5.6
python3 scripts/run_fixtures.py fixtures -- python3 pipeline/pipeline.py
```

## Submitting and scoring

Submit your public GitHub repository on the Buildathon track page. Enter a commit SHA to score a specific revision, or leave it empty to score the newest commit on the default branch.

The image's `CMD` runs with no arguments. Exit code 0 means "done, check my output". Build time is never timed; only the run is timed. The track page lists the current practice limits and how the final is scored.

## Container limits

Scorers are Google Cloud n2 Intel Xeon machines, Cascade Lake or newer, with AVX-512 available. Your container gets:

| Resource | Setting |
| --- | --- |
| `/data` | The dataset, read-only |
| `/output` | Empty, writable. Write `/output/user_daily/` and `/output/campaign_daily/` as Parquet |
| `/scratch` | Empty, writable, fast local disk for spills and temp files |
| CPU / RAM | 8 vCPUs, hyperthreads of an Intel Xeon, 24 GB, no swap |
| Network | None |
| Env vars | `DATA_DIR=/data`, `OUTPUT_DIR=/output`, `SCRATCH_DIR=/scratch`, `HOME=/scratch`, `NCPU=8`, `MEM_GB=24` |
| Filesystem | Root is read-only apart from `/tmp`, 64 MB in memory. The process runs as uid 1000, not root |

## Common pitfalls

- **DuckDB's CSV reader differs from `rules.md`.** It reads a quoted empty field `""` as an empty string, not null. Its malformed-line handling, including `ignore_errors`, `strict_mode`, and `null_padding`, does not match `rules.md`. Test against the fixtures before relying on it.
- **Memory.** `pipeline.py` sets `memory_limit` to 80% of `MEM_GB` and `temp_directory` under `/scratch`, so big joins and sorts spill to disk instead of getting the container killed.
- **No extension downloads.** The container has no network at run time. Parquet and CSV are built into the wheel; anything else must be installed at build time.
- **`\d` is not `[0-9]`.** In several regex engines, including Rust's used by Polars, `\d` matches any Unicode digit. The rules mean ASCII digits only.
- **Extra files in output folders.** `_SUCCESS` and other non-Parquet files in the output folders are ignored.

## Self-checks

- Summed over all rows, `sessions` and `revenue_cents` are equal in both tables.
- `day` must be a Parquet `DATE`, not a string or timestamp.
- Row order and the number of Parquet files do not matter. Exact column names do, and there must be no extra columns.

## Files in this starter

- `pipeline/pipeline.py` holds your pipeline code.
- `Dockerfile` builds a `linux/amd64` image for the Intel Xeon scorers.
- `fixtures/` holds the 36 edge cases, each an `input/` folder and the `expected/` tables. In this monorepo they live in `../../fixtures`; copy them in when publishing the starter as its own repo.
- `scripts/` runs your image the way the scorer does and diffs the output.
