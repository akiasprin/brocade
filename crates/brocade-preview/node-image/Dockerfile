# 一个镜像跑所有节点：xray + WireGuard 用户态工具 + 基本网络工具。
#
# 用 Ubuntu 24.04 而不是 Alpine，这是 preview 这条路独有的约束：preview 把宿主机
# `cargo build -p brocade-agent` 的产物直接发进容器，那份是动态链接 glibc 的
# （当前引用到 GLIBC_2.39，Ubuntu 24.04 正好提供），Alpine/musl 上会在安装脚本
# 探测阶段报 “not found”。
#
# 控制面走的是另一条路：`brocade-console/build.rs` 把静态 musl agent 编进控制面，
# 由 `/enroll/dist` 分发，不受 glibc 约束。
FROM ubuntu:24.04

# nftables 是 phantun 那条路的硬依赖：agent 配 NAT 用的是 `nft`（phantun.rs
# 的 install_phantun_nat），而 Ubuntu 的 `iptables` 包只给 iptables 命令——
# 虽然它自己走的就是 nf_tables 后端，`nft` 这个命令仍在单独的包里。
# 少了它，凡是入站传输选了伪 TCP 的机器，收敛到 phantun 那一步就报
# 「需要 nft（nftables）来给 phantun 配 NAT」。
RUN apt-get update \
 && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
      wireguard-tools bash iproute2 iptables nftables \
      curl dnsutils tcpdump jq procps ca-certificates unzip wget \
      iputils-ping \
 && rm -rf /var/lib/apt/lists/*

ARG XRAY_VER=v26.3.27
RUN wget -qO /tmp/x.zip "https://github.com/XTLS/Xray-core/releases/download/${XRAY_VER}/Xray-linux-64.zip" \
 && mkdir -p /opt/xray \
 && unzip -o -q /tmp/x.zip -d /opt/xray xray geoip.dat geosite.dat \
 && chmod +x /opt/xray/xray && ln -s /opt/xray/xray /usr/local/bin/xray \
 && rm /tmp/x.zip
ENV XRAY_LOCATION_ASSET=/opt/xray

# phantun：伪 TCP 那条路的两个二进制。
#
# agent 找的是 `/usr/local/bin/phantun-server` / `-client`（连字符，见 phantun.rs
# 的 ensure_phantun_binary），找不到就退而求其次用控制面下发的分发源；两条都没有
# 就报「不在这台机器上，控制面也没配 phantun 分发源」，那台机器的发布直接 failed。
# preview 这条路没有分发源可配，所以跟 xray 一样把二进制烤进镜像。
#
# 官方产物叫 `phantun_server` / `phantun_client`（下划线），跟 agent 要的名字差一个
# 字符，所以装的时候要改名——照搬解压结果的话，agent 照样找不到。
# 取 musl 版：静态链接，不受基础镜像的 glibc 版本牵连。
ARG PHANTUN_VER=v0.8.1
RUN wget -qO /tmp/p.zip "https://github.com/dndx/phantun/releases/download/${PHANTUN_VER}/phantun_x86_64-unknown-linux-musl.zip" \
 && unzip -o -q /tmp/p.zip -d /tmp/phantun \
 && install -m 0755 /tmp/phantun/phantun_server /usr/local/bin/phantun-server \
 && install -m 0755 /tmp/phantun/phantun_client /usr/local/bin/phantun-client \
 && rm -rf /tmp/p.zip /tmp/phantun

COPY entrypoint.sh /entrypoint.sh
RUN chmod +x /entrypoint.sh
ENTRYPOINT ["/entrypoint.sh"]
