FROM ubuntu:24.04
RUN apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
       ca-certificates curl iproute2 procps python3 \
    && rm -rf /var/lib/apt/lists/*
