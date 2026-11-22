
# =====================================================
# Engine builders
# =====================================================

FROM rust:1-bookworm AS cpp_builder

RUN apt-get update \
    && apt-get install -y --no-install-recommends g++ cmake nlohmann-json3-dev \
    && rm -rf /var/lib/apt/lists/*
COPY cpp_engine/ /build/cpp_engine/
RUN cmake -S /build/cpp_engine -B /build/cpp -DCMAKE_BUILD_TYPE=Release -DCMAKE_INTERPROCEDURAL_OPTIMIZATION=ON \
    && cmake --build /build/cpp --parallel 4 \
    && ctest --test-dir /build/cpp --output-on-failure

FROM rust:1-bookworm AS rust_builder

WORKDIR /build

COPY rust_engine/Cargo.toml rust_engine/Cargo.lock /build/rust_engine/
COPY rust_engine/src/ /build/rust_engine/src/

RUN cargo build \
    --locked \
    --release \
    --manifest-path /build/rust_engine/Cargo.toml

# =====================================================
# STAGE 2: Python + DuckDB runtime
# =====================================================

FROM python:3.12-slim-bookworm AS output_runtime

RUN useradd \
    --uid 1000 \
    --no-create-home \
    --home-dir /scratch \
    app

WORKDIR /app

COPY requirements.txt /app/requirements.txt

RUN pip install \
    --no-cache-dir \
    -r /app/requirements.txt

COPY pipeline/ /app/pipeline/
COPY run.sh /app/run.sh

# Remove Windows BOM and CRLF from run.sh.
RUN python3 -c 'from pathlib import Path; p=Path("/app/run.sh"); data=p.read_bytes().removeprefix(bytes.fromhex("efbbbf")).replace(b"\r\n", b"\n"); p.write_bytes(data)' \
    && chmod 755 /app/run.sh

ENV PYTHONDONTWRITEBYTECODE=1
ENV PYTHONUNBUFFERED=1

# IMPORTANT:
# Start the script explicitly with the Linux shell.
CMD ["/bin/sh", "/app/run.sh"]

FROM output_runtime AS rust
COPY --from=rust_builder /build/rust_engine/target/release/rust_engine /app/rust_engine
ENV SESSION_ENGINE=/app/rust_engine

FROM output_runtime AS cpp
COPY --from=cpp_builder /build/cpp/cpp_engine /app/cpp_engine
ENV SESSION_ENGINE=/app/cpp_engine

FROM cpp AS cpp_mmap
ENV SESSION_MMAP=1

FROM cpp_mmap AS competition
