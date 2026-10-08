# Internet Money node and miner.
#   docker build -t imoney .
#   docker run -d --name imoney -v imoney-data:/data -p 18555:18555 -p 127.0.0.1:18556:18556 imoney
FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p imoney-node -p imoney-miner -p imoney-stratum

FROM debian:bookworm-slim
RUN useradd --system --create-home --home-dir /data imoney
COPY --from=build /src/target/release/imoney-node /src/target/release/imoney-miner /src/target/release/imoney-stratum /usr/local/bin/
USER imoney
VOLUME /data
# 18555: peer-to-peer (open to the internet). 18556: RPC and wallet (keep private).
EXPOSE 18555 18556
ENTRYPOINT ["imoney-node", "--data-dir", "/data", "--p2p-bind", "0.0.0.0:18555", "--rpc-bind", "0.0.0.0:18556"]
