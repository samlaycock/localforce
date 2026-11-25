# syntax=docker/dockerfile:1

FROM rust:1.99-trixie AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src src
COPY ui/dist ui/dist
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked --bin localforce \
    && cp target/release/localforce /usr/local/bin/localforce
RUN mkdir /data /project

# Run: distroless as a non-root user. The org (SQLite) and the HTTPS
# certificate live in /data. Mount an SFDX project at /project and set
# LOCALFORCE_METADATA=/project/force-app to load its objects and fields, and
# a config file at /localforce.toml (or set LOCALFORCE_CONFIG) for users.
FROM gcr.io/distroless/cc-debian13:nonroot
COPY --from=build /usr/local/bin/localforce /usr/local/bin/localforce
COPY --from=build --chown=nonroot:nonroot /data /data
COPY --from=build /project /project
ENV LOCALFORCE_HOST=0.0.0.0 \
    LOCALFORCE_DATA_DIR=/data
VOLUME /data
EXPOSE 9600 9643
ENTRYPOINT ["/usr/local/bin/localforce"]
