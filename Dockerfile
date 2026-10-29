# backend-proxy のイメージ（backend-rust の compose から使う）。
FROM rust:1.98.1-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p shop-proxy

FROM debian:bookworm-slim
COPY --from=build /src/target/release/shop-proxy /usr/local/bin/shop-proxy
USER nobody
EXPOSE 50051
CMD ["shop-proxy"]
