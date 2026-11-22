# Final Shopping Sessions Competition Submission

## Submission candidate

Repository: https://github.com/vishusinghsohi/shopping-sessions-optimized

The **competition** Docker target is the intended submission. It is the direct C++ to Arrow/Parquet engine. Do **not** submit the `cpp`, `cpp_mmap`, or `rust` comparison targets.

## Reproduce the scorer image

```bash
docker build --platform linux/amd64 --target competition -t shopping-sessions:final .
```

The competition image runs **without command-line arguments**:

```
/app/cpp_engine
```

It expects the scorer to mount read-only `/data`, writable `/output`, and writable `/scratch`. The scorer supplies `DATA_DIR=/data`, `OUTPUT_DIR=/output`, `SCRATCH_DIR=/scratch`, `NCPU=8`, and `MEM_GB=24`.

## Required checks BEFORE scoring

1. Open **Actions > Competition release verification**. Confirm a **green pass on the exact commit SHA** you intend to submit. If no run exists, select **Run workflow** on `main`. A workflow file being present is not evidence it passed.
2. Ensure the image builds successfully for `linux/amd64` with the `competition` target.
3. Ensure CTest succeeds during the image build and the fixture runner passes **36/36** official cases.
4. Ensure the container runs with no network, a read-only root filesystem, uid 1000, 8 vCPUs, and a 24-GB memory cap.
5. Check both output folders contain correctly typed Parquet: `/output/user_daily/` and `/output/campaign_daily/`. Compare against the provided expected results, not just exit code.
6. Verify the public 1M-event dev dataset if available. On a 60M-event workload, verify peak memory stays safely below 24 GB.
7. Confirm a clean tree and copy the commit SHA from GitHub's commit view; **do not substitute a previous commit's SHA** after making changes.

### Manual commands (Linux)

```bash
python3 -m pip install 'polars>=1.30,<2'
docker build --platform linux/amd64 --target competition -t shopping-sessions:final .
IMAGE=shopping-sessions:final PLATFORM=linux/amd64 CPUS=8 MEM=24g \
  python3 scripts/run_fixtures.py fixtures -- sh scripts/docker_run.sh
```

The fixture script checks exact output values and the Parquet schemas. It fails on mismatches.

### Manual commands (Windows PowerShell)

```powershell
docker build --platform linux/amd64 --target competition -t shopping-sessions:final .
python -m pip install "polars>=1.30,<2"
$env:IMAGE = "shopping-sessions:final"
python scripts/run_fixtures.py fixtures -- python scripts/docker_run_windows.py
```

## Score the submission

On the Shopping Sessions Buildathon track page submit:

- Repository: `https://github.com/vishusinghsohi/shopping-sessions-optimized`
- Commit SHA: **the verified passing commit** from `main`.

Do not claim a new official runtime from local or CI benchmarks. Local timing excludes remote scorer conditions; the hidden competition workload is authoritative.

## Hold conditions

Do **not** call this revision fully validated until GitHub Actions reports the fixture job succeeded and the official scorer accepts its output. If the action cannot start or a fixture fails, resolve that before submission. Keep the last known accepted revision available as a fallback.
