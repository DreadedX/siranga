FROM rust:1.85 AS base
ENV CARGO_REGISTRIES_CRATES_IO_PROTOCOL=sparse
RUN cargo install cargo-chef --locked --version 0.1.71 && \
    cargo install cargo-auditable --locked --version 0.6.6
WORKDIR /app

FROM base AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM base AS builder
COPY --from=planner /app/recipe.json recipe.json
ENV RUSTC_BOOTSTRAP=1
RUN cargo chef cook --release --recipe-path recipe.json

COPY . .
ENV RUSTC_BOOTSTRAP=1
RUN cargo auditable build --release

FROM gcr.io/distroless/cc-debian12:nonroot AS runtime
COPY --from=builder /app/target/release/tunnel_rs /tunnel_rs
CMD ["/tunnel_rs"]
