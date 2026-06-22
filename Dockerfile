# picklejar AI-memory server: build the Postgres-wire binary, ship a slim image.
#
#   docker build -t picklejar .
#   docker run -p 5433:5433 -v picklejar-data:/data picklejar
#   psql -h 127.0.0.1 -p 5433 -U postgres      # or any Postgres driver

FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release --bin picklejar-pg

FROM debian:bookworm-slim
RUN useradd --system --create-home picklejar
COPY --from=build /src/target/release/picklejar-pg /usr/local/bin/picklejar-pg
# Persist the database under /data (mount a volume here).
RUN mkdir -p /data && chown picklejar /data
USER picklejar
VOLUME ["/data"]
EXPOSE 5433
# Bind 0.0.0.0 so the port is reachable from outside the container.
ENTRYPOINT ["picklejar-pg", "--host", "0.0.0.0", "--port", "5433", "--database", "/data/picklejar.db"]
