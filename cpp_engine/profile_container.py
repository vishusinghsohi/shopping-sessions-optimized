import subprocess
import time
from pathlib import Path


def cgroup(filename, key=None):
    try:
        data = (Path("/sys/fs/cgroup") / filename).read_text()
        if key is not None:
            data = dict(line.split() for line in data.splitlines())[key]
        return int(data)
    except (OSError, KeyError, ValueError):
        return None


cpu_before = cgroup("cpu.stat", "usage_usec")
started = time.perf_counter()
subprocess.run(["/bin/sh", "/app/run.sh"], check=True)
elapsed = time.perf_counter() - started
cpu_after = cgroup("cpu.stat", "usage_usec")
peak = cgroup("memory.peak")
print(f"[MEASURE] inner_wall_seconds {elapsed:.6f}", flush=True)
if cpu_before is not None and cpu_after is not None:
    print(f"[MEASURE] cpu_seconds {(cpu_after - cpu_before) / 1_000_000:.6f}", flush=True)
if peak is not None:
    print(f"[MEASURE] container_peak_mib {peak / 2**20:.3f}", flush=True)
