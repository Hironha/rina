FROM rust:1.74.1 as builder

RUN apt update && \
    apt install libssl-dev \
        libopus-dev -y

WORKDIR /app

COPY . .

# build in release mode
RUN cargo build --target x86_64-unknown-linux-gnu --release

FROM debian:12.4

WORKDIR /app

COPY --from=builder /app/target/x86_64-unknown-linux-gnu/release/rina ./
COPY --from=builder /app/.env ./

RUN apt update && \
    apt install libssl-dev \
        libopus-dev \
        ffmpeg \
        youtube-dl -y

CMD [ "/app/rina" ]