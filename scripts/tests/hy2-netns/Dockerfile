FROM ubuntu:24.04
RUN rm -f /etc/apt/apt.conf.d/docker-clean \
 && mkdir /packages \
 && apt-get update \
 && DEBIAN_FRONTEND=noninteractive apt-get -o Dir::Cache::archives=/packages install -y --no-install-recommends \
      iproute2 nftables iptables conntrack python3 tcpdump \
 && rm -rf /var/lib/apt/lists/*
