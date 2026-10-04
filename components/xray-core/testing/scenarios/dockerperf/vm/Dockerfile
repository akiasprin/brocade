FROM brocade-arm-vm-tools:latest

RUN apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        iproute2 \
        qemu-system-x86 \
    && rm -rf /var/lib/apt/lists/*
