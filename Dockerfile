FROM rust:1.78-alpine

WORKDIR /app
ENV RUSTFLAGS="-Ctarget-feature=-crt-static"

RUN apk add --update pkgconf musl-dev openssl-dev

COPY ./src src
COPY ./Cargo.toml Cargo.toml

RUN cargo install --path .

FROM alpine:latest

RUN apk add --update libgcc openssl
COPY --from=0 /usr/local/cargo/bin/crates-spider /bin/crates-spider

CMD ["crates-spider"]