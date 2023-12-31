FROM rust:1.74.1 as builder

# install thirdy party dependencies needed at compile time
RUN apt-get update && \
    apt-get install -y libssl-dev libopus-dev && \
    rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY . .

# build in release mode
RUN cargo build --target x86_64-unknown-linux-gnu --release

FROM debian:12.4

# install thirdy party dependencies needed to run songbird
RUN apt-get update && \
    apt-get install -y libssl-dev libopus-dev python3 python3-pip && \
    rm -rf /var/lib/apt/lists/* && \
    pip3 install --upgrade --break-system-packages yt-dlp

WORKDIR /app

COPY --from=builder /app/target/x86_64-unknown-linux-gnu/release/rina ./
COPY --from=builder /app/.env ./

CMD [ "/app/rina" ]