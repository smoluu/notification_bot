# Build stage
FROM rust:1.89 AS build

WORKDIR /notification_bot

# copy manifests for dependency caching
COPY ./Cargo.toml ./Cargo.lock ./

# create a dummy source file to cache dependencies
RUN mkdir src && echo "fn main() {}" > src/main.rs
RUN cargo build --release
# remove temp binary and source file
RUN rm -rf target/release/notification_bot src

# copy source files
COPY ./src ./src

# build with source files
RUN cargo build --release

# runtime stage
FROM ubuntu:24.04

# install runtime dependencies
RUN apt-get update && apt-get install -y \
    iputils-ping \
    libssl-dev \
    ca-certificates \
    nmap \
    && rm -rf /var/lib/apt/lists/*

# Copy the binary
COPY --from=build /notification_bot/target/release/notification_bot /usr/local/bin/notification_bot
# make config folder & copy config file file
RUN mkdir -p /etc/notification_bot
COPY ./hosts.txt /etc/notification_bot/hosts.txt
RUN chmod +x /usr/local/bin/notification_bot

# Run the binary
CMD ["notification_bot"]