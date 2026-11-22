
#!/bin/sh

set -eu

: "${DATA_DIR:=/data}"
: "${OUTPUT_DIR:=/output}"
: "${SCRATCH_DIR:=/scratch}"

export DATA_DIR OUTPUT_DIR SCRATCH_DIR

# The scorer's root filesystem is read-only.
# Keep temporary files and the home directory
# inside its writable scratch volume.
export TMPDIR="$SCRATCH_DIR"
export HOME="$SCRATCH_DIR"

cd "$SCRATCH_DIR"

exec python3 /app/pipeline/pipeline.py