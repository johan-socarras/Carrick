# A relay of your own for Carrick: the official iroh-relay server, built from
# its published source, in a small image. See relay/README.md.
#
#   docker build -t carrick-relay relay/
#
# The build compiles iroh-relay; CARGO_BUILD_JOBS keeps it from taking every core.

FROM rust:1-trixie AS build
ARG IROH_RELAY_VERSION=1.5.0
ARG CARGO_BUILD_JOBS=4
RUN cargo install iroh-relay --version "${IROH_RELAY_VERSION}" --features server --locked --root /out

FROM debian:trixie-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates openssl \
 && rm -rf /var/lib/apt/lists/*
COPY --from=build /out/bin/iroh-relay /usr/local/bin/iroh-relay
COPY start.sh /usr/local/bin/carrick-relay
RUN chmod +x /usr/local/bin/carrick-relay
VOLUME /data
# 80: plain HTTP (and the Let's Encrypt challenge), 443: the relay itself,
# 7842/udp: lets each computer learn its public address (QUIC).
EXPOSE 80/tcp 443/tcp 7842/udp
ENTRYPOINT ["/usr/local/bin/carrick-relay"]
