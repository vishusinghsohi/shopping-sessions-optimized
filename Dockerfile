
# =====================================================
# Engine builders
# =====================================================

FROM rust:1-bookworm AS cpp_toolchain

RUN apt-get update \
    && apt-get install -y --no-install-recommends g++ cmake nlohmann-json3-dev \
    && rm -rf /var/lib/apt/lists/*

FROM cpp_toolchain AS cpp_builder
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

FROM python:3.12-slim-bookworm AS arrow_sdk

RUN pip install --no-cache-dir --no-deps --only-binary=:all: --target /wheel pyarrow==23.0.1 \
    && mkdir -p /arrow/lib \
    && cp -r /wheel/pyarrow/include /arrow/include \
    && cp /wheel/pyarrow/libarrow.so.2300 /wheel/pyarrow/libparquet.so.2300 /arrow/lib/ \
    && ln -s libarrow.so.2300 /arrow/lib/libarrow.so \
    && ln -s libparquet.so.2300 /arrow/lib/libparquet.so \
    && cp -r /wheel/pyarrow-*.dist-info/licenses /arrow/licenses

FROM cpp_toolchain AS native_builder
COPY cpp_engine/ /build/cpp_engine/
COPY --from=arrow_sdk /arrow /arrow
RUN cmake -S /build/cpp_engine -B /build/native -DCMAKE_BUILD_TYPE=Release \
        -DCMAKE_INTERPROCEDURAL_OPTIMIZATION=ON -DSHOPPING_ARROW_ROOT=/arrow \
    && cmake --build /build/native --parallel 4 \
    && ctest --test-dir /build/native --output-on-failure \
    && cmake --install /build/native --prefix /native

FROM debian:bookworm-slim AS competition
RUN apt-get update \
    && apt-get install -y --no-install-recommends libstdc++6 \
    && rm -rf /var/lib/apt/lists/*
COPY --from=arrow_sdk /arrow/lib/*.so.2300 /app/lib/
COPY --from=arrow_sdk /arrow/licenses /app/licenses/arrow/
COPY --from=native_builder /native/cpp_engine /app/cpp_engine
ENV SESSION_MMAP=1
CMD ["/app/cpp_engine"]
