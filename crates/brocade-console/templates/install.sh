#!/bin/sh
set -eu

# Mind a blind spot of `set -e`: **inside a function tested by `||` or `if`, `set -e` does not
# apply**. `install_xray || exit 1` and `install_phantun || echo …` in this script are both such
# calls, so every command in those two functions that can fail must check its own return value.
# Missing one costs not a failed installation but a broken one: after a failed curl the empty
# temporary file is installed as xray anyway, the symptom is `Exec format error`, and that is worse
# than having done nothing.

SERVER=
ENROLL_TOKEN=${BROCADE_ENROLL_TOKEN:-}
NODE_TOKEN_ARG=${BROCADE_NODE_TOKEN:-}
AGENT_BIN_URL=${BROCADE_AGENT_BIN_URL:-}
AGENT_BIN_SHA256=${BROCADE_AGENT_BIN_SHA256:-}
XRAY_BIN_URL=${BROCADE_XRAY_BIN_URL:-}
XRAY_BIN_SHA256=${BROCADE_XRAY_BIN_SHA256:-}
if [ -n "${BROCADE_XRAY_VERSION:-}" ]; then XRAY_VERSION_EXPLICIT=1; else XRAY_VERSION_EXPLICIT=; fi
XRAY_VERSION=${BROCADE_XRAY_VERSION:-v26.4.25}
if [ -n "$XRAY_BIN_URL" ]; then XRAY_BIN_URL_EXPLICIT=1; else XRAY_BIN_URL_EXPLICIT=; fi
PHANTUN_BIN_URL=${BROCADE_PHANTUN_BIN_URL:-}
PHANTUN_BIN_SHA256=${BROCADE_PHANTUN_BIN_SHA256:-}
PHANTUN_VERSION=${BROCADE_PHANTUN_VERSION:-latest}
# These four can come from the command line or from the control plane's manifest; under set -u they
# need a default first
PHANTUN_SERVER_URL=${BROCADE_PHANTUN_SERVER_URL:-}
PHANTUN_SERVER_SHA=${BROCADE_PHANTUN_SERVER_SHA256:-}
PHANTUN_CLIENT_URL=${BROCADE_PHANTUN_CLIENT_URL:-}
PHANTUN_CLIENT_SHA=${BROCADE_PHANTUN_CLIENT_SHA256:-}
APPLY_MODE=${BROCADE_AGENT_APPLY:-}
if [ -n "$APPLY_MODE" ]; then APPLY_MODE_EXPLICIT=1; else APPLY_MODE_EXPLICIT=; fi
SERVICE_MODE=${BROCADE_AGENT_SERVICE_MODE:-auto}
ENABLE_VPNGATE=${BROCADE_ENABLE_VPNGATE:-0}
if [ -n "${BROCADE_VPNGATE_STATS_WINDOW_SECS:-}" ]; then
    VPNGATE_STATS_WINDOW_EXPLICIT=1
else
    VPNGATE_STATS_WINDOW_EXPLICIT=
fi
VPNGATE_STATS_WINDOW_SECS=${BROCADE_VPNGATE_STATS_WINDOW_SECS:-900}
INSTALL_DIR=${BROCADE_AGENT_INSTALL_DIR:-/usr/local/bin}
CONFIG_DIR=${BROCADE_AGENT_CONFIG_DIR:-/etc/brocade-agent}
STATE_DIR=${BROCADE_AGENT_STATE_DIR:-/var/lib/brocade-agent}
TMPFILES=

# An interactive terminal shows a download progress bar; redirected output or CI stays silent.
# Note that --progress-bar and -s are mutually exclusive (the latter wins), so the tty branch has to
# drop -s while keeping -f -S -L. The progress bar goes to stderr, as this script's other notices
# do.
if [ -t 1 ]; then
    CURL_FLAGS='-fSL --progress-bar'
else
    CURL_FLAGS='-fsSL'
fi

cleanup() {
    [ -n "$TMPFILES" ] && rm -f $TMPFILES
    return 0
}
trap cleanup EXIT INT TERM

usage() {
    echo "usage: sh brocade-install.sh --server URL [--enroll-token TOKEN | --node-token TOKEN]" >&2
    echo "  首次纳管用 --enroll-token（一次性）" >&2
    echo "  token 轮换 / 机器重装用 --node-token（控制面重签时会给出这条完整命令）" >&2
    echo "  都不带 = 沿用机器上已有的 token，纯升级" >&2
    echo "         [--apply linux|state-dir]" >&2
    echo "         [--service-mode auto|systemd|openrc|foreground]" >&2
    echo "         [--enable-openvpn]" >&2
    echo "         [--vpngate-stats-window-secs SECONDS]" >&2
    echo "         [--agent-bin-url URL] [--agent-bin-sha256 SHA256]" >&2
    echo "         [--xray-bin-url URL] [--xray-bin-sha256 SHA256] [--xray-version TAG]" >&2
    echo "         [--phantun-server-url URL] [--phantun-server-sha256 SHA256]" >&2
    echo "         [--phantun-client-url URL] [--phantun-client-sha256 SHA256]" >&2
    echo >&2
    echo "  --apply linux      默认。把产物真的落到系统上：配 wg0、拉起 xray" >&2
    echo "  --apply state-dir  只把产物写进状态目录，不碰系统。lab 用，生产别用" >&2
    echo "  --service-mode auto        默认。自动选择 systemd 或 OpenRC" >&2
    echo "  --service-mode systemd     强制写 systemd unit" >&2
    echo "  --service-mode openrc      强制写 OpenRC service" >&2
    echo "  --service-mode foreground  不写服务，直接 exec agent。preview 容器用" >&2
    echo "  --enable-openvpn            安装 OpenVPN/iptables，加载并持久化 TUN；默认不安装" >&2
    echo "  --vpngate-stats-window-secs 本机 VPN Gate 性能统计窗口；默认 900，范围 60–86400 秒" >&2
}

have() { command -v "$1" >/dev/null 2>&1; }

install_pkg() {
    if have apt-get; then
        DEBIAN_FRONTEND=noninteractive apt-get update -qq >/dev/null 2>&1 || true
        DEBIAN_FRONTEND=noninteractive apt-get install -y -qq "$1" >/dev/null
    elif have dnf; then
        dnf install -y -q "$1" >/dev/null
    elif have yum; then
        yum install -y -q "$1" >/dev/null
    elif have apk; then
        apk add --no-cache "$1" >/dev/null
    elif have pacman; then
        pacman -Sy --noconfirm "$1" >/dev/null
    elif have zypper; then
        zypper --non-interactive install -y "$1" >/dev/null
    elif have xbps-install; then
        xbps-install -Sy "$1" >/dev/null
    elif have emerge; then
        emerge --quiet "$1" >/dev/null
    else
        return 1
    fi
}

tun_device_usable() {
    [ -c /dev/net/tun ] || return 1
    # Merely seeing a device node is insufficient in a restricted container: the devices cgroup
    # may still reject opening it. Opening and immediately closing the clone device creates no
    # interface, but proves that OpenVPN will at least reach TUNSETIFF instead of failing at open(2).
    (: <> /dev/net/tun) 2>/dev/null
}

ensure_tun_runtime() {
    if ! tun_device_usable; then
        # A built-in TUN driver already exposes this sysfs entry. Otherwise load the module before
        # creating the device node; doing it in the opposite order can leave a convincing but dead
        # /dev/net/tun behind on kernels that do not provide the driver.
        if [ ! -r /sys/class/misc/tun/dev ]; then
            if ! have modprobe; then
                echo "installing kmod for the VPN Gate TUN driver ..." >&2
                install_pkg kmod || {
                    echo "无法安装 modprobe；不能加载 VPN Gate 所需的 tun 内核模块" >&2
                    return 1
                }
            fi
            if ! modprobe tun; then
                echo "当前内核无法加载 tun 模块；VPN Gate 的 OpenVPN 隧道无法创建" >&2
                return 1
            fi
        fi

        # devtmpfs normally creates this automatically. Minimal containers and a few VPS images
        # expose the kernel misc device without populating /dev/net, so recover only from the
        # kernel-advertised major/minor pair instead of hard-coding an unverified device node.
        if [ ! -c /dev/net/tun ] && [ -r /sys/class/misc/tun/dev ]; then
            tun_device_number=$(cat /sys/class/misc/tun/dev 2>/dev/null || true)
            tun_major=${tun_device_number%%:*}
            tun_minor=${tun_device_number#*:}
            case "$tun_major:$tun_minor" in
                *[!0-9:]*|:|*:)
                    echo "内核返回了无效的 TUN 设备号：${tun_device_number:-空}" >&2
                    return 1
                    ;;
            esac
            if [ -e /dev/net/tun ]; then
                echo "/dev/net/tun 已存在但不是字符设备；拒绝覆盖" >&2
                return 1
            fi
            install -d -m 0755 /dev/net || return 1
            mknod /dev/net/tun c "$tun_major" "$tun_minor" || return 1
            chmod 0666 /dev/net/tun || return 1
        fi
    fi

    if ! tun_device_usable; then
        echo "/dev/net/tun 不可用；可能是容器未透传设备或宿主机禁止 TUN" >&2
        return 1
    fi

    # Both systemd-modules-load and Alpine/OpenRC consume this directory. Keep it separate from
    # brocade.conf: tune_conntrack owns that file and rewrites it on every installer run.
    install -d -m 0755 /etc/modules-load.d || return 1
    tun_module_stage=/etc/modules-load.d/.brocade-vpngate.$$
    TMPFILES="$TMPFILES $tun_module_stage"
    printf 'tun\n' > "$tun_module_stage" || return 1
    chmod 0644 "$tun_module_stage" || return 1
    mv -f "$tun_module_stage" /etc/modules-load.d/brocade-vpngate.conf || return 1
    echo "VPN Gate TUN 已就绪，并会在开机时加载。" >&2
}

# Download, verify, install. **A failure at any step must not touch $dest.**
# Installing nothing is safer than replacing a working binary with a partial download.
fetch_binary() {
    url=$1
    want_sha=$2
    dest=$3
    tmp=$(mktemp)
    TMPFILES="$TMPFILES $tmp"
    if ! curl $CURL_FLAGS "$url" -o "$tmp"; then
        echo "下载失败：$url（$dest 保持原样）" >&2
        return 1
    fi
    if [ ! -s "$tmp" ]; then
        echo "下载到的是空文件：$url（$dest 保持原样）" >&2
        return 1
    fi
    if [ -n "$want_sha" ]; then
        if ! printf '%s  %s\n' "$want_sha" "$tmp" | sha256sum -c - >/dev/null 2>&1; then
            echo "sha256 不匹配：$url（$dest 保持原样）" >&2
            return 1
        fi
    fi
    install_binary_atomic "$tmp" "$dest"
}

# Stage beside the destination and rename only after the complete file has its final mode. This
# keeps a running binary intact across disk-full, permission, and interrupted-copy failures.
install_binary_atomic() {
    src=$1
    dest=$2
    stage=$(mktemp "$(dirname "$dest")/.brocade-install.XXXXXX") || return 1
    TMPFILES="$TMPFILES $stage"
    install -m 0755 "$src" "$stage" || return 1
    mv -f "$stage" "$dest"
}

# Xray is a Brocade-owned build artifact. The normal source is this Console's embedded binary;
# there is deliberately no community release fallback because future Brocade builds will carry
# protocol changes that upstream Xray does not.
# The first fallback directory xray searches for assets (GetAssetLocation in common/platform).
XRAY_ASSET_DIR=${BROCADE_XRAY_ASSET_DIR:-/usr/local/share/xray}
# Where the .dat files come from, the same source as the control plane's geodata defaults (the copy
# Xray's own release workflow pulls).
GEODATA_BASE=${BROCADE_GEODATA_BASE:-https://raw.githubusercontent.com/Loyalsoldier/v2ray-rules-dat/release}

# Whether the installed binary is exactly the pinned release. The tag carries a leading `v`
# and the banner does not, so compare on the bare number; anything unparseable answers "no",
# which costs one redundant download and never leaves a machine silently off the pin.
xray_version_is() {
    have=$(printf '%s' "$1" | sed -n 's/.*[Xx]ray[ v]*\([0-9][0-9.]*\).*/\1/p')
    want=${2#v}
    [ -n "$have" ] && [ "$have" = "$want" ]
}

# Confirm the machine really ended up on the pinned release, and refuse the installation if not.
#
# The check exists because the agent launches `xray` by name, not by path: whatever the PATH
# resolves to is what serves traffic. A distribution package at /usr/bin/xray, or an image with one
# baked in, can therefore win over the copy just written to $INSTALL_DIR — leaving a machine that
# installed cleanly, reports nothing wrong, and runs a version the fleet is pinned away from. That
# is the failure this pin exists to prevent, so it must not be possible to walk out of the installer
# with it.
#
# Checked here rather than left to the agent: restarting a serving process because its version looks
# wrong is a far heavier act than refusing an installation, and installation is the moment where
# nothing is running yet and failing is free.
#
# An explicitly supplied URL is the operator escape hatch. The normal Console-selected path must
# report the pinned version and have exactly the sha256 published by this Console; the Xray banner
# remains upstream's and carries no Brocade marker.
verify_xray_pin() {
    [ -z "$XRAY_BIN_URL_EXPLICIT" ] || return 0

    actual_path=$(command -v xray 2>/dev/null || echo "$XRAY_BIN")
    running=$("$actual_path" version 2>/dev/null | grep -i xray | head -n 1)
    actual_sha=$(sha256sum "$actual_path" 2>/dev/null | cut -d' ' -f1)
    if xray_version_is "$running" "$XRAY_VERSION" \
       && [ -n "$XRAY_BIN_SHA256" ] && [ "$actual_sha" = "$XRAY_BIN_SHA256" ]; then
        return 0
    fi
    echo "机队要求 Brocade Xray $XRAY_VERSION，但这台上会被执行的是：" >&2
    echo "  $actual_path -> ${running:-（问不出版本）}" >&2
    if [ "$actual_path" != "$XRAY_BIN" ]; then
        echo "PATH 先找到的不是刚装的那份（$XRAY_BIN）。agent 是按名字起 xray 的，" >&2
        echo "所以真正服务的会是上面那个。把它移开、或让 $INSTALL_DIR 排在 PATH 前面。" >&2
    else
        echo "刚装的那份版本或 sha256 不对；拒绝把非 Console 分发的字节当作机队版本。" >&2
    fi
    return 1
}

# geoip.dat / geosite.dat must be present, **regardless of whether a rule table uses them**.
# And missing .dat files are not a soft failure of rules not matching. `geosite:` is expanded at
# **config parse time** into `ext:geosite.dat:` and the file read on the spot (infra/conf/router.go →
# geodata.ParseDomainRules), and failing to read it gives:
#   Failed to start: … common/geodata: failed to open geosite.dat
# That is, xray **does not start**. So they are a precondition for a machine working at all, not an
# optional dependency of one class of rule.
ensure_geodata() {
    need=
    [ -s "$XRAY_ASSET_DIR/geoip.dat" ] || need="$need geoip.dat"
    [ -s "$XRAY_ASSET_DIR/geosite.dat" ] || need="$need geosite.dat"
    [ -n "$need" ] || return 0

    echo "缺规则库（$need），下载中 ..." >&2
    mkdir -p "$XRAY_ASSET_DIR"
    for f in $need; do
        if curl -fsSL --max-time 120 "$GEODATA_BASE/$f" -o "$XRAY_ASSET_DIR/$f.tmp" \
           && [ -s "$XRAY_ASSET_DIR/$f.tmp" ]; then
            mv "$XRAY_ASSET_DIR/$f.tmp" "$XRAY_ASSET_DIR/$f"
        else
            rm -f "$XRAY_ASSET_DIR/$f.tmp"
            echo "  下不到 $f——这台机器上带 geosite:/geoip: 的配置会让 xray 起不来" >&2
            return 1
        fi
    done
    return 0
}

install_xray() {
    if [ -z "$XRAY_BIN_URL" ]; then
        if [ -z "$DIST_JSON" ]; then
            echo "问不到 $SERVER/enroll/dist，拿不到 Brocade Xray；不会回退下载社区 Xray。" >&2
            echo "请确认 --server 指向控制面的节点入口，或显式传 --xray-bin-url。" >&2
        elif [ -z "$XRAY_ARCH" ]; then
            echo "控制面只内嵌 x86_64 和 aarch64 的 Brocade Xray，本机是 $(uname -m)。" >&2
            echo "请为这个架构构建 Brocade Xray，并用 --xray-bin-url 明确指定。" >&2
        else
            echo "控制面的分发清单缺少 $XRAY_ARCH 的 Brocade Xray；拒绝回退社区版本。" >&2
        fi
        return 1
    fi
    if [ -n "$XRAY_BIN_SHA256" ] && [ -s "$XRAY_BIN" ] \
       && printf '%s  %s\n' "$XRAY_BIN_SHA256" "$XRAY_BIN" | sha256sum -c - >/dev/null 2>&1; then
        return 0
    fi
    echo "downloading Brocade Xray ($XRAY_VERSION) ..." >&2
    fetch_binary "$XRAY_BIN_URL" "$XRAY_BIN_SHA256" "$XRAY_BIN"
}

# phantun: WireGuard is UDP only, and this wears a TCP disguise for it when an upstream seals inbound
# UDP. The same three tiers as xray: a pinned URL, then what is already on the machine, then the
# release page.
#
# Failing to install it is not fatal: only nodes the model really gives fake TCP need it, and the
# vast majority of machines do not. Where one really needs it and it is absent, the agent reports
# plainly that phantun-server is not on PATH when starting the instance and health turns red — better
# than stopping the whole installation here.
install_phantun() {
    # Distributed by the control plane (preferred): each binary has its own URL and sha
    if [ -n "$PHANTUN_SERVER_URL" ] && [ -n "$PHANTUN_CLIENT_URL" ]; then
        fetch_binary "$PHANTUN_SERVER_URL" "$PHANTUN_SERVER_SHA" "$PHANTUN_SERVER_BIN" || return 1
        fetch_binary "$PHANTUN_CLIENT_URL" "$PHANTUN_CLIENT_SHA" "$PHANTUN_CLIENT_BIN" || return 1
        return 0
    fi
    if [ -n "$PHANTUN_BIN_URL" ]; then
        fetch_binary "$PHANTUN_BIN_URL" "$PHANTUN_BIN_SHA256" "$PHANTUN_SERVER_BIN" || return 1
        return 0
    fi
    # Reading the output again: an empty file's --help also "succeeds"
    if "$PHANTUN_SERVER_BIN" --help 2>/dev/null | grep -qi phantun \
        && "$PHANTUN_CLIENT_BIN" --help 2>/dev/null | grep -qi phantun; then
        return 0
    fi

    case "$(uname -m)" in
        x86_64|amd64) triple=x86_64-unknown-linux-musl ;;
        aarch64|arm64) triple=aarch64-unknown-linux-musl ;;
        armv7l|armv7) triple=armv7-unknown-linux-musleabihf ;;
        i686|i386) triple=i686-unknown-linux-musl ;;
        *)
            echo "不认识的架构 $(uname -m)，需要伪 TCP 的话用 --phantun-server-url / --phantun-client-url 指定" >&2
            return 1
            ;;
    esac
    if [ "$PHANTUN_VERSION" = "latest" ]; then
        base="https://github.com/dndx/phantun/releases/latest/download"
    else
        base="https://github.com/dndx/phantun/releases/download/$PHANTUN_VERSION"
    fi

    # The release page gives a zip holding the client and server binaries, not two bare files
    echo "downloading phantun ($triple) ..." >&2
    zip=$(mktemp)
    TMPFILES="$TMPFILES $zip"
    if ! curl $CURL_FLAGS "$base/phantun_$triple.zip" -o "$zip"; then
        echo "下不到 phantun_$triple.zip，需要伪 TCP 的话用 --phantun-server-url / --phantun-client-url 指定" >&2
        return 1
    fi
    if ! have unzip; then
        install_pkg unzip || {
            echo "需要 unzip 解开 phantun 发行包，且无法自动安装" >&2
            return 1
        }
    fi
    dir=$(mktemp -d)
    if ! unzip -oq "$zip" -d "$dir"; then
        echo "phantun 发行包解不开" >&2
        rm -rf "$dir"
        return 1
    fi
    # Inside the archive they are phantun_client / phantun_server (underscores) and land as the
    # hyphenated names: the agent pkills by process name and the two must agree (stop_phantun in
    # main.rs)
    for prog in server client; do
        src=$(find "$dir" -type f -name "phantun_$prog" | head -1)
        if [ -z "$src" ]; then
            echo "phantun 发行包里找不到 phantun_$prog" >&2
            rm -rf "$dir"
            return 1
        fi
        install_binary_atomic "$src" "$INSTALL_DIR/phantun-$prog" || { rm -rf "$dir"; return 1; }
    done
    rm -rf "$dir"
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --server)
            SERVER=${2:-}
            shift 2
            ;;
        --enroll-token)
            ENROLL_TOKEN=${2:-}
            shift 2
            ;;
        --node-token)
            NODE_TOKEN_ARG=${2:-}
            shift 2
            ;;
        --apply)
            APPLY_MODE=${2:-}
            APPLY_MODE_EXPLICIT=1
            shift 2
            ;;
        --service-mode)
            SERVICE_MODE=${2:-}
            shift 2
            ;;
        --enable-openvpn)
            ENABLE_VPNGATE=1
            shift
            ;;
        --vpngate-stats-window-secs)
            VPNGATE_STATS_WINDOW_SECS=${2:-}
            VPNGATE_STATS_WINDOW_EXPLICIT=1
            shift 2
            ;;
        --agent-bin-url)
            AGENT_BIN_URL=${2:-}
            shift 2
            ;;
        --agent-bin-sha256)
            AGENT_BIN_SHA256=${2:-}
            shift 2
            ;;
        --xray-bin-url)
            XRAY_BIN_URL=${2:-}
            XRAY_BIN_URL_EXPLICIT=1
            shift 2
            ;;
        --xray-version)
            XRAY_VERSION=${2:-}
            XRAY_VERSION_EXPLICIT=1
            shift 2
            ;;
        --xray-bin-sha256)
            XRAY_BIN_SHA256=${2:-}
            shift 2
            ;;
        --phantun-server-url)
            PHANTUN_SERVER_URL=${2:-}
            shift 2
            ;;
        --phantun-server-sha256)
            PHANTUN_SERVER_SHA=${2:-}
            shift 2
            ;;
        --phantun-client-url)
            PHANTUN_CLIENT_URL=${2:-}
            shift 2
            ;;
        --phantun-client-sha256)
            PHANTUN_CLIENT_SHA=${2:-}
            shift 2
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            usage
            exit 2
            ;;
    esac
done

if [ -z "$SERVER" ]; then
    usage
    exit 2
fi
# On an upgrade, omission means "keep what this machine was using", not "switch to linux". An
# explicit environment value or --apply still wins; only a truly unspecified value is inherited.
if [ -z "$APPLY_MODE_EXPLICIT" ] && [ -r "$CONFIG_DIR/env" ]; then
    APPLY_MODE=$(sed -n 's/^BROCADE_AGENT_APPLY=//p' "$CONFIG_DIR/env" | tail -n 1)
fi
if [ -z "$VPNGATE_STATS_WINDOW_EXPLICIT" ] && [ -r "$CONFIG_DIR/env" ]; then
    inherited_vpngate_stats_window=$(sed -n 's/^BROCADE_VPNGATE_STATS_WINDOW_SECS=//p' "$CONFIG_DIR/env" | tail -n 1)
    if [ -n "$inherited_vpngate_stats_window" ]; then
        VPNGATE_STATS_WINDOW_SECS=$inherited_vpngate_stats_window
    fi
fi
[ -n "$APPLY_MODE" ] || APPLY_MODE=linux
# Re-running this script on an already enrolled machine is an upgrade: replace the binaries, fill in
# the dependencies, refresh the env and the unit, and require no further enrollment token (it is
# single-use and a second cannot be signed).
# Three credential routes, in order of precedence: a directly issued long-lived token, a one-time
# enrollment, then whatever the machine already has. The middle one is a first enrollment, and the
# first covers a re-signed token, a lost token, and a reinstalled machine.
if [ -n "$NODE_TOKEN_ARG" ]; then
    CRED_MODE=node-token
elif [ -n "$ENROLL_TOKEN" ]; then
    CRED_MODE=enroll
elif [ -s "$CONFIG_DIR/token" ]; then
    CRED_MODE=reuse
else
    echo "这台还没纳管过：首次要 --enroll-token，或用 --node-token 直接下发控制面签好的 token" >&2
    usage
    exit 2
fi
case "$APPLY_MODE" in
    linux|state-dir) ;;
    *)
        echo "unknown --apply value: $APPLY_MODE (expected linux or state-dir)" >&2
        exit 2
        ;;
esac
case "$SERVICE_MODE" in
    auto|systemd|openrc|foreground) ;;
    *)
        echo "unknown --service-mode value: $SERVICE_MODE (expected auto, systemd, openrc, or foreground)" >&2
        exit 2
        ;;
esac
case "$ENABLE_VPNGATE" in
    0|1) ;;
    *)
        echo "BROCADE_ENABLE_VPNGATE must be 0 or 1" >&2
        exit 2
        ;;
esac
case "$VPNGATE_STATS_WINDOW_SECS" in
    ''|*[!0-9]*)
        echo "BROCADE_VPNGATE_STATS_WINDOW_SECS must be an integer between 60 and 86400" >&2
        exit 2
        ;;
esac
if [ "$VPNGATE_STATS_WINDOW_SECS" -lt 60 ] || [ "$VPNGATE_STATS_WINDOW_SECS" -gt 86400 ]; then
    echo "BROCADE_VPNGATE_STATS_WINDOW_SECS must be between 60 and 86400 seconds" >&2
    exit 2
fi

SERVER=$(printf '%s' "$SERVER" | sed 's:/*$::')
if [ "$(id -u)" != "0" ]; then
    echo "brocade install must run as root" >&2
    exit 1
fi
if ! have curl; then
    echo "curl is required" >&2
    exit 1
fi

# Resolve the default before enrollment consumes a one-time token. Failing here leaves the token
# usable and avoids the old state where all files were installed but no init could start them.
if [ "$SERVICE_MODE" = "auto" ]; then
    if have systemctl && [ -d /run/systemd/system ]; then
        SERVICE_MODE=systemd
    elif have rc-service && have rc-update && have supervise-daemon && [ -x /sbin/openrc-run ]; then
        SERVICE_MODE=openrc
    else
        echo "找不到受支持的服务管理器：需要 systemd，或带 supervise-daemon 的 OpenRC。" >&2
        echo "容器内如有外部 supervisor，可显式使用 --service-mode foreground。" >&2
        exit 1
    fi
fi
if [ "$SERVICE_MODE" = "systemd" ]; then
    if ! have systemctl || [ ! -d /run/systemd/system ]; then
        echo "指定了 --service-mode systemd，但 systemd 没有在运行。" >&2
        exit 1
    fi
fi
if [ "$SERVICE_MODE" = "openrc" ]; then
    missing_openrc=
    for command in rc-service rc-update supervise-daemon; do
        have "$command" || missing_openrc="$missing_openrc $command"
    done
    [ -x /sbin/openrc-run ] || missing_openrc="$missing_openrc /sbin/openrc-run"
    if [ -n "$missing_openrc" ]; then
        echo "指定了 --service-mode openrc，但缺少:$missing_openrc" >&2
        echo "Alpine 上请先安装/恢复 openrc 包。" >&2
        exit 1
    fi
fi

# Fail before downloading binaries or consuming a one-time enrollment token. OpenVPN can be
# perfectly installed while every probe fails later if the kernel TUN driver was never loaded — a
# particularly common shape on Alpine's virt kernel. Explicit VPN Gate opt-in promises the complete
# capability, so current and post-reboot TUN availability are both mandatory here.
if [ "$APPLY_MODE" = "linux" ] && [ "$ENABLE_VPNGATE" = "1" ]; then
    ensure_tun_runtime || exit 1
fi

install -d -m 0755 "$INSTALL_DIR" "$CONFIG_DIR" "$STATE_DIR"
AGENT_BIN="$INSTALL_DIR/brocade-agent"
XRAY_BIN="$INSTALL_DIR/xray"
PHANTUN_SERVER_BIN="$INSTALL_DIR/phantun-server"
PHANTUN_CLIENT_BIN="$INSTALL_DIR/phantun-client"

# With the arguments incomplete, ask the control plane for a distribution manifest.
#
# The command for a first enrollment is assembled by the control plane and carries a string of
# --agent-bin-url arguments; but **whoever re-runs the script usually holds only --server** — that
# original command is long lost. Without the URLs the binary already on the machine is kept and a new
# unit written below, producing a new unit configured against an old binary and a service that never
# starts again.
#
# Output only where a `"key": "value"` really matched. `sed -n ...p` rather than `s///`: the latter
# emits the whole line unchanged on no match, so null becomes the literal
# `"xray_bin_url":null}`, is taken for a URL and fed to curl, and on a brand-new machine fails
# install_xray and aborts the whole script.
dist_field() {
    printf '%s' "$DIST_JSON" | tr ',' '\n' \
        | sed -n 's/.*"'"$1"'" *: *"\([^"]*\)".*/\1/p' | head -1
}
# Command-line values always win and the manifest only fills blanks. A control plane that cannot be
# asked counts as absent rather than an error: specifying --*-bin-url by hand, or the machine already
# having them, are both legitimate ways to install.
DIST_JSON=
if curl -fsSL "$SERVER/enroll/dist" -o /tmp/brocade-dist.$$ 2>/dev/null; then
    DIST_JSON=$(cat /tmp/brocade-dist.$$)
    TMPFILES="$TMPFILES /tmp/brocade-dist.$$"
fi
# The host architecture, used to select matching embedded artifacts from the manifest.
case "$(uname -m)" in
    x86_64|amd64) AGENT_ARCH=x86_64; XRAY_ARCH=x86_64 ;;
    aarch64|arm64) AGENT_ARCH=aarch64; XRAY_ARCH=aarch64 ;;
    # The control plane embeds only these two architectures. No approximation is guessed for
    # others — what gets installed is an Exec format error, a symptom several layers from the real
    # cause. It is left empty and reported plainly below.
    *) AGENT_ARCH=; XRAY_ARCH= ;;
esac

if [ -n "$DIST_JSON" ]; then
    # What the operator configured explicitly wins (a CDN, or a fleet holding architectures the
    # embedding does not cover); unconfigured, the control plane's own copy is selected by host
    # architecture.
    [ -n "$AGENT_BIN_URL" ]              || AGENT_BIN_URL=$(dist_field agent_bin_url)
    [ -n "$AGENT_BIN_SHA256" ]           || AGENT_BIN_SHA256=$(dist_field agent_bin_sha256)
    if [ -z "$AGENT_BIN_URL" ] && [ -n "$AGENT_ARCH" ]; then
        AGENT_BIN_URL=$(dist_field "agent_bin_url_$AGENT_ARCH")
        AGENT_BIN_SHA256=$(dist_field "agent_bin_sha256_$AGENT_ARCH")
    fi
    if [ -z "$XRAY_VERSION_EXPLICIT" ]; then
        XRAY_VERSION=$(dist_field xray_version)
        [ -n "$XRAY_VERSION" ] || XRAY_VERSION=v26.4.25
    fi
    if [ -z "$XRAY_BIN_URL" ]; then
        XRAY_BIN_URL=$(dist_field xray_bin_url)
        if [ -n "$XRAY_BIN_URL" ]; then
            XRAY_BIN_URL_EXPLICIT=1
            XRAY_BIN_SHA256=$(dist_field xray_bin_sha256)
        elif [ -n "$XRAY_ARCH" ]; then
            XRAY_BIN_URL=$(dist_field "xray_bin_url_$XRAY_ARCH")
            XRAY_BIN_SHA256=$(dist_field "xray_bin_sha256_$XRAY_ARCH")
        fi
    fi
    [ -n "$PHANTUN_SERVER_URL" ]         || PHANTUN_SERVER_URL=$(dist_field phantun_server_url)
    [ -n "$PHANTUN_SERVER_SHA" ]         || PHANTUN_SERVER_SHA=$(dist_field phantun_server_sha256)
    [ -n "$PHANTUN_CLIENT_URL" ]         || PHANTUN_CLIENT_URL=$(dist_field phantun_client_url)
    [ -n "$PHANTUN_CLIENT_SHA" ]         || PHANTUN_CLIENT_SHA=$(dist_field phantun_client_sha256)
fi

if [ -n "$AGENT_BIN_URL" ]; then
    fetch_binary "$AGENT_BIN_URL" "$AGENT_BIN_SHA256" "$AGENT_BIN" || exit 1
elif [ -x "$AGENT_BIN" ]; then
    echo "没有 --agent-bin-url，也问不到控制面的分发清单：沿用机器上已有的 $AGENT_BIN" >&2
elif have brocade-agent; then
    install_binary_atomic "$(command -v brocade-agent)" "$AGENT_BIN"
else
    # Reaching here means all three routes failed: no URL was given, the machine has no agent, and
    # the manifest did not fill the gap. There are two reasons the manifest could not, pointing at
    # entirely different fixes, so they are reported separately:
    #   DIST_JSON empty     — curl got nothing. Usually --server points at the admin port (8080 by
    #                         default), while /enroll/dist exists only on the agent port (8081 by
    #                         default) and 8080 answers 404.
    #   DIST_JSON non-empty — the control plane answered without agent_bin_url configured (console
    #                         has no BROCADE_AGENT_BIN_URL set).
    if [ -z "$DIST_JSON" ]; then
        echo "问不到 $SERVER/enroll/dist 的分发清单——--server 要对到控制面的 agent 端口（默认 8081）；" >&2
        echo "admin 端口（默认 8080）上没有这个路径，直接 404。" >&2
    elif [ -z "$AGENT_ARCH" ]; then
        echo "控制面答了 $SERVER/enroll/dist，但它只内嵌 x86_64 和 aarch64 两个架构的 agent，" >&2
        echo "而本机是 $(uname -m)。在这个架构上自己编一个再指过来：" >&2
        echo "  cargo build --release --target <triple> -p brocade-agent" >&2
        echo "  sh brocade-install.sh --server $SERVER --agent-bin-url <URL>" >&2
    else
        echo "控制面答了 $SERVER/enroll/dist，但清单里没有 $AGENT_ARCH 的 agent——" >&2
        echo "这个控制面编的时候没带上这个架构（正常情况下 build.rs 会强制带上）。" >&2
    fi
    echo "brocade-agent binary not found; rerun with --agent-bin-url or set BROCADE_AGENT_BIN_URL" >&2
    exit 1
fi

# The binary must support the command the unit will use, or the unit written below never starts.
#
# The test reads its own usage output: fed a nonexistent subcommand, it lists what it supports.
# --server and --token must both be supplied, because argument validation precedes command
# dispatch — with one missing, all one sees is a missing argument.
#
# Self-update gates on this same output before it replaces a running binary
# (brocade-agent/src/selfupdate.rs, `probe`). The two must keep agreeing: loosen it there and a
# binary this installer would refuse can still arrive by the other road, onto a machine nobody can
# log in to.
probe_out=$("$AGENT_BIN" --server http://127.0.0.1:1 --token probe __brocade_probe 2>&1 || true)
if ! printf '%s' "$probe_out" | grep -qE 'expected[^|]*\brun\b'; then
    # Distinguish an executable/architecture failure from a command-contract mismatch so the
    # operator gets the right repair instructions.
    case "$probe_out" in
        *"Exec format error"*|*"cannot execute"*|*"not found"*|"")
            echo "「$AGENT_BIN」跑不起来（本机架构 $(uname -m)）。" >&2
            echo "控制面分发的 agent 二进制多半不是这个架构的——" >&2
            echo "在这个架构上自己编一个，用 --agent-bin-url 指过去：" >&2
            echo "  cargo build --release --target <triple> -p brocade-agent" >&2
            ;;
        *)
            echo "这个 brocade-agent 不符合当前命令契约：缺少 'run'（$AGENT_BIN）。" >&2
            echo "unit 会用 'run' 启动，写下去只会起不来——先把二进制换掉：" >&2
            echo "  重跑本脚本并带上 --agent-bin-url <控制面地址>/brocade-agent" >&2
            echo "  或让控制面配好 BROCADE_AGENT_BIN_URL，脚本会自己去 \$SERVER/enroll/dist 取" >&2
            ;;
    esac
    exit 1
fi

# linux mode really touches the system, so the data-plane dependencies are installed here in one go
# rather than leaving anything to be prepared by hand first.
if [ "$APPLY_MODE" = "linux" ]; then
    if ! have wg || ! have wg-quick; then
        echo "installing wireguard-tools ..." >&2
        install_pkg wireguard-tools || true
    fi
    install_xray || exit 1
    verify_xray_pin || exit 1
    # Reaching here, xray is certainly present; the .dat files are confirmed once more — there are
    # several installation paths and only this one is taken by all of them.
    ensure_geodata || exit 1
    # Failing to install it does not block the installation: only nodes given fake TCP need it, and
    # the agent and health report it missing faithfully
    install_phantun || echo "phantun 没装上；这台若要用伪 TCP 需手动补" >&2
    # nftables serves two features that write no firewall rules themselves: phantun (DNAT inbound
    # TCP to the TUN, masquerade what leaves it — the `nft -f -` in phantun.rs) and Hysteria 2 port
    # hopping (fold a UDP range onto the one port that listens — porthop.rs). Without the command
    # either one fails at convergence, on the machine, well after this script has reported success.
    #
    # Best-effort rather than fatal, for the same reason as phantun: most machines carry neither,
    # and refusing to install over a missing packet filter would turn away the common case to serve
    # the rare one. The package is named nftables on every manager install_pkg knows.
    if ! have nft; then
        echo "installing nftables ..." >&2
        install_pkg nftables || true
    fi
    # OpenVPN is deliberately not part of a normal Agent installation. A machine opts into the
    # capability explicitly; once installed, the Agent reports the exact version and the control
    # plane may offer it VPN Gate work. Failure is fatal only for this explicit opt-in.
    if [ "$ENABLE_VPNGATE" = "1" ]; then
        if ! have openvpn; then
            echo "installing openvpn for managed VPN Gate exits ..." >&2
            install_pkg openvpn || {
                echo "无法安装 openvpn；这台机器不会加入 VPN Gate 接入节点" >&2
                exit 1
            }
        fi
        if ! have iptables; then
            echo "installing iptables for managed VPN Gate namespace NAT ..." >&2
            install_pkg iptables || {
                echo "无法安装 iptables；VPN Gate 隔离命名空间无法联网" >&2
                exit 1
            }
        fi
    fi

    missing=
    for c in wg wg-quick ip pgrep pkill; do
        have "$c" || missing="$missing $c"
    done
    if ! have xray && [ ! -x "$XRAY_BIN" ]; then
        missing="$missing xray"
    fi
    if [ -n "$missing" ]; then
        echo "--apply linux 需要这些命令，自动安装后仍然缺失:$missing" >&2
        echo "这台机器的包管理器不认识（或被禁用）。装好 wireguard-tools 后重跑，" >&2
        echo "或用 --xray-bin-url 让控制面分发 xray。" >&2
        exit 1
    fi
    # Warned about rather than counted as missing, for the reason above — it is needed only where the
    # model gives this machine fake TCP. Said here all the same: the alternative to hearing it now is
    # hearing it from a node that has already been enrolled and cannot bring its tunnel up.
    have nft || echo "nft（nftables）没装上；这台若要用伪 TCP 或 Hysteria 2 端口跳转，规则配不上" >&2
    if [ "$ENABLE_VPNGATE" = "1" ]; then
        have openvpn || { echo "--enable-openvpn 需要 openvpn" >&2; exit 1; }
        have iptables || { echo "--enable-openvpn 需要 iptables" >&2; exit 1; }
    fi
else
    if [ -n "$XRAY_BIN_URL" ]; then
        fetch_binary "$XRAY_BIN_URL" "$XRAY_BIN_SHA256" "$XRAY_BIN" || exit 1
    fi
fi

umask 077
if [ "$CRED_MODE" = "node-token" ]; then
    printf '%s\n' "$NODE_TOKEN_ARG" > "$CONFIG_DIR/token"
    NODE_ID="(已写入下发的 token)"
elif [ "$CRED_MODE" = "enroll" ]; then
    auth_header=$(mktemp)
    TMPFILES="$TMPFILES $auth_header"
    chmod 0600 "$auth_header"
    printf 'Authorization: Bearer %s\n' "$ENROLL_TOKEN" > "$auth_header"
    response=$(curl -fsS -X POST -H "@$auth_header" "$SERVER/agent/v1/enroll")
    NODE_TOKEN=$(printf '%s\n' "$response" | sed -n 's/.*"token"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p')
    NODE_ID=$(printf '%s\n' "$response" | sed -n 's/.*"node_id"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p')
    if [ -z "$NODE_TOKEN" ] || [ -z "$NODE_ID" ]; then
        echo "enrollment response did not contain node token" >&2
        exit 1
    fi
    printf '%s\n' "$NODE_TOKEN" > "$CONFIG_DIR/token"
else
    NODE_ID="(已纳管)"
    echo "沿用 $CONFIG_DIR/token，跳过纳管" >&2
fi
cat > "$CONFIG_DIR/env" <<EOF
BROCADE_AGENT_SERVER=$SERVER
BROCADE_NODE_TOKEN_FILE=$CONFIG_DIR/token
BROCADE_AGENT_STATE_DIR=$STATE_DIR
BROCADE_AGENT_APPLY=$APPLY_MODE
BROCADE_XRAY_BIN=$XRAY_BIN
BROCADE_VPNGATE_STATS_WINDOW_SECS=$VPNGATE_STATS_WINDOW_SECS
EOF
chmod 0600 "$CONFIG_DIR/token" "$CONFIG_DIR/env"

# 把这台机器的拥塞控制切到 BBR。
#
# 为什么值得在装机时就做：跨境、高时延又有丢包的线路上，cubic 会被丢包一路压到抬不起头，
# 而 BBR 不看丢包看带宽时延积。brocade 的每一跳都是终结重发的 TCP（xray 收下再拨出去），
# 所以每一跳都吃得到——这跟纯 IP 转发不一样，路由器上 BBR 一个字节的差别都没有。
#
# 还有第二个理由，不那么直觉：cubic **不维护瓶颈带宽估计**。控制台上「这一跳多宽、
# 哪一跳是瓶颈」那几列是从内核的 tcp_bbr_info 读出来的，不开 BBR 就永远是空的。
# 所以这不只是让它跑得快，也是让它看得见。
#
# 三种机器改不了，而且都是真实存在的：OpenVZ 和部分 LXC 的 sysctl 是只读的（低价 VPS
# 里一抓一把）；4.9 以下的内核根本没有 BBR（CentOS 7 的 3.10 至今有人在跑）；
# 极少数精简镜像没把 tcp_bbr 编进去。这三种情况下**继续装**——机器照样能跑 brocade，
# 只是慢一点；缺少这项可选优化不应阻止安装。
tune_congestion() {
    # set -e 在被 `|| true` 包住的函数里不生效（见文件顶部），所以下面每一步自己判返回值。
    kernel=$(uname -r 2>/dev/null || echo 0.0)
    major=${kernel%%.*}
    rest=${kernel#*.}
    minor=${rest%%.*}
    case "$major$minor" in
        *[!0-9]*|'') major=0; minor=0 ;;
    esac
    if [ "$major" -lt 4 ] || { [ "$major" -eq 4 ] && [ "$minor" -lt 9 ]; }; then
        echo "内核 $kernel 早于 4.9，没有 BBR。这台改不了，只能换镜像重装。" >&2
        return 0
    fi

    # 内建时必然失败，无所谓——下一步的 available 列表才是判据。
    modprobe tcp_bbr >/dev/null 2>&1 || true

    available=$(cat /proc/sys/net/ipv4/tcp_available_congestion_control 2>/dev/null || echo '')
    case " $available " in
        *" bbr "*) ;;
        *)
            echo "这个内核没有 bbr（可选的只有：${available:-读不到}）。跳过，不影响安装。" >&2
            return 0
            ;;
    esac

    # 写文件而不是只 sysctl -w：只 -w 的话重启就没了，而「装的时候设过、现在不是了」
    # 正是控制台要报的那种漂移——设了但不持久，等于给自己埋一个假信号。
    if ! cat > /etc/sysctl.d/99-brocade.conf <<'SYSCTL'
# Brocade: TCP transport defaults.
net.core.default_qdisc = fq
net.ipv4.tcp_congestion_control = bbr
SYSCTL
    then
        echo "写不了 /etc/sysctl.d/99-brocade.conf，跳过 BBR。不影响安装。" >&2
        return 0
    fi

    sysctl --system >/dev/null 2>&1 || sysctl -p /etc/sysctl.d/99-brocade.conf >/dev/null 2>&1 || true

    # 回读核对。写进去 ≠ 生效：OpenVZ 上 sysctl 是只读的，写文件会成功而值纹丝不动。
    now_cc=$(cat /proc/sys/net/ipv4/tcp_congestion_control 2>/dev/null || echo '')
    if [ "$now_cc" != "bbr" ]; then
        echo "设了 bbr 但当前仍是「${now_cc:-读不到}」——多半是 OpenVZ/LXC，sysctl 只读。" >&2
        echo "不影响安装，这台机器会比别的慢一档，而且控制台上量不出它的带宽。" >&2
        rm -f /etc/sysctl.d/99-brocade.conf
        return 0
    fi

    # fq 单独判定，因为它可能一个成一个败：sysctl 的权限往往是逐 key 的。
    #
    # 4.13 起 BBR 不再硬性需要 fq（TCP 有内建 pacing），所以设不上不算失败。仍然设，是因为
    # fq 把 pacing 卸到 qdisc 层，并发流一多就省 CPU——而 CPU 正是转发机上先于带宽耗尽的那个。
    nic=$(awk '$2 == "00000000" { print $1; exit }' /proc/net/route 2>/dev/null || echo '')
    if [ -n "$nic" ] && have tc; then
        # 关键的一步，也是最容易漏的：改 default_qdisc 只对**之后新建**的队列生效，
        # 已经 up 的网卡纹丝不动。只回读 default_qdisc 的话它显示 fq、而 eth0 上还是
        # fq_codel——「设了但没生效」会被判成成功。所以这里直接换现网卡上的那个。
        tc qdisc replace dev "$nic" root fq >/dev/null 2>&1 || true
    fi

    echo "已开启 BBR（拥塞控制 bbr + qdisc fq）。" >&2
}
tune_congestion || true

# TCP Fast Open has two independent kernel bits: 1 lets this machine initiate TFO and 2 lets its
# listeners accept it. Xray still enables the option per socket; this switch merely allows those
# AnyTLS/VLESS socket settings to take effect. Keep it separate from BBR because a machine without
# BBR can still support TFO, and failing either optimisation must not prevent installation.
tune_tcp_fast_open() {
    conf=/etc/sysctl.d/99-brocade.conf
    if [ ! -e "$conf" ]; then
        if ! printf '%s\n' '# Brocade: TCP transport defaults.' > "$conf"; then
            echo "写不了 $conf，跳过 TCP Fast Open。不影响安装。" >&2
            return 0
        fi
    fi
    if ! sed -i '/^[[:space:]]*net\.ipv4\.tcp_fastopen[[:space:]]*=/d' "$conf"; then
        echo "更新不了 $conf，跳过 TCP Fast Open。不影响安装。" >&2
        return 0
    fi
    if ! printf '%s\n' 'net.ipv4.tcp_fastopen = 3' >> "$conf"; then
        echo "写不了 TCP Fast Open 配置，跳过。不影响安装。" >&2
        return 0
    fi

    sysctl -qw net.ipv4.tcp_fastopen=3 >/dev/null 2>&1 || true
    now_tfo=$(cat /proc/sys/net/ipv4/tcp_fastopen 2>/dev/null || echo '')
    case "$now_tfo" in
        *[!0-9]*|'') now_tfo=0 ;;
    esac
    if [ $((now_tfo & 3)) -ne 3 ]; then
        echo "设了 TCP Fast Open 但当前值仍是「$now_tfo」——多半是容器限制或内核不支持。跳过，不影响安装。" >&2
        sed -i '/^[[:space:]]*net\.ipv4\.tcp_fastopen[[:space:]]*=/d' "$conf" 2>/dev/null || true
        return 0
    fi
    echo "已开启 TCP Fast Open（客户端 + 服务端）。" >&2
}
tune_tcp_fast_open || true

# AnyTLS can move TLS 1.3 AES-GCM record processing into the kernel. Xray keeps kTLS in `auto`, so
# an old or restricted kernel still works through ordinary Go TLS; the installer only makes a
# usable module available now and after reboot. The kTLS writev path is selected by Xray without
# host setup; the separate pipe-splice experiment stays independently switchable because its result
# varies with the kernel and workload. Keep the module file separate from brocade.conf because
# tune_conntrack rewrites that file on every installation.
enable_xray_ktls() {
    if [ ! -d /sys/module/tls ]; then
        if ! have modprobe; then
            echo "installing kmod for optional Xray kTLS support ..." >&2
            if ! install_pkg kmod; then
                echo "无法安装 modprobe；AnyTLS 将继续使用 Go TLS，不影响安装。" >&2
                return 0
            fi
        fi
        if ! modprobe tls >/dev/null 2>&1; then
            echo "当前内核无法加载 tls 模块；AnyTLS 将继续使用 Go TLS，不影响安装。" >&2
            return 0
        fi
    fi

    if [ ! -d /sys/module/tls ]; then
        echo "tls 模块加载后仍不可见；AnyTLS 将继续使用 Go TLS，不影响安装。" >&2
        return 0
    fi

    if ! install -d -m 0755 /etc/modules-load.d; then
        echo "写不了 /etc/modules-load.d；kTLS 当前可用，但重启后可能回退到 Go TLS。" >&2
        return 0
    fi
    ktls_module_stage=/etc/modules-load.d/.brocade-xray-ktls.$$
    TMPFILES="$TMPFILES $ktls_module_stage"
    if ! printf 'tls\n' > "$ktls_module_stage" ||
       ! chmod 0644 "$ktls_module_stage" ||
       ! mv -f "$ktls_module_stage" /etc/modules-load.d/brocade-xray-ktls.conf; then
        echo "持久化 tls 模块失败；kTLS 当前可用，但重启后可能回退到 Go TLS。" >&2
        return 0
    fi
    echo "Xray kTLS 模块已就绪，并会在开机时加载（运行时仍按 auto 安全回退）。" >&2
}
enable_xray_ktls || true

# 连接跟踪表。写进同一个 99-brocade.conf，但单独一个函数——两者的失败条件不一样，而且
# BBR 设不上的那批机器（OpenVZ/LXC、老内核）恰恰是内存最小、最先撞上连接表上限的那批，
# 让它们因为没有 BBR 就连这个也拿不到是反的。
#
# # 为什么非设不可
#
# nf_conntrack_max 是内核按内存推出来的：431MB 的机器推出来 3584 条。而 hy2 端口跳转每换
# 一个目的端口就是一条新表项——这个特性自己就是压力源，且 redirect 属于 NAT、NAT 离不开
# conntrack，摘不掉。表满之后内核丢的是**新**流，已经建立的照常走，所以症状是「跳转突然
# 不好使了、重启一下又行」而不是整台断，最难往连接表上想。已经在一台 431MB 的机器上量到
# 过打满（3584/3584）。
#
# # 为什么必须连模块一起管
#
# net.netfilter.* 这些 key 只在 nf_conntrack 已加载时才存在。装机时机器上通常还没有任何
# nat 表，模块没加载，systemd-sysctl 会静默跳过这几行；等 agent 后来装上 nft 表把模块带
# 起来，值又回到内核默认。modules-load.d 让它在 systemd-sysctl 之前加载——两步缺一，重启
# 一次这些值就没了，而且没有任何报错。
tune_conntrack() {
    # set -e 在被 `|| true` 包住的函数里不生效（见文件顶部），每一步自己判返回值。
    modprobe nf_conntrack >/dev/null 2>&1 || true
    current=$(cat /proc/sys/net/netfilter/nf_conntrack_max 2>/dev/null || echo '')
    case "$current" in
        ''|*[!0-9]*)
            echo "读不到 nf_conntrack_max，这台没有连接跟踪（模块缺失或 sysctl 只读）。跳过，不影响安装。" >&2
            return 0
            ;;
    esac

    # 取现值和 32768 的较大者，不是硬写一个数：内存大的机器内核已经给到 65536，往下压是倒
    # 退。32768 条约 10MB，在 431MB 的机器上占 2.4%，是能接受的代价。
    want=32768
    if [ "$current" -gt "$want" ]; then
        want=$current
    fi

    # udp_timeout_stream 是这里的主角：默认 120 秒，而端口跳转换过去的旧目的端口是**立刻**
    # 废弃的，压两分钟纯粹是占位。砍到 60 秒直接把占用减半。
    #
    # 不砍得更狠是因为回程：落点回包时 conntrack 负责把源端口改回客户端发往的那个跳转端口，
    # 表项一旦过期，服务端主动发的包就带着监听口的源端口出去，客户端那边 QUIC 会认不出来。
    # hysteria2 的保活是十秒级，60 秒留了六倍余量。要再紧可以调，但那是拿余量换表容量。
    conf=/etc/sysctl.d/99-brocade.conf
    # 先删掉本函数以前写过的行再追加，否则重装一次就多一份。sysctl 是后者覆盖前者，重复不
    # 会出错，但一个越读越长的文件会让人以为有人手工改过。
    #
    # grep 一行都没输出时返回 1，而「文件里只剩 conntrack 行」正是重装一台 BBR 设不上的机器
    # 时的常态——照着返回值判成失败就会跳过清理，然后追加，每重装一次多一份。所以这里只看
    # 临时文件在不在：空文件也是正确结果。
    if [ -f "$conf" ]; then
        grep -v \
            -e '^[[:space:]]*net\.netfilter\.nf_conntrack_max[[:space:]]*=' \
            -e '^[[:space:]]*net\.netfilter\.nf_conntrack_udp_timeout[[:space:]]*=' \
            -e '^[[:space:]]*net\.netfilter\.nf_conntrack_udp_timeout_stream[[:space:]]*=' \
            -e '^# Brocade: conntrack for Hysteria 2 port hopping\.$' \
            -e '^# 连接跟踪。hy2 端口跳转' \
            -e '^# 机器上是三四千条。' \
            "$conf" > "$conf.tmp" 2>/dev/null || true
        if [ -f "$conf.tmp" ]; then
            mv "$conf.tmp" "$conf" 2>/dev/null || rm -f "$conf.tmp"
        fi
    fi
    if ! cat >> "$conf" <<SYSCTL
# Brocade: conntrack for Hysteria 2 port hopping.
net.netfilter.nf_conntrack_max = $want
net.netfilter.nf_conntrack_udp_timeout = 30
net.netfilter.nf_conntrack_udp_timeout_stream = 60
SYSCTL
    then
        echo "写不了 $conf，跳过连接表调整。不影响安装。" >&2
        return 0
    fi

    # 模块要在 systemd-sysctl 之前加载，见上面的说明。写不了就只是重启后失效，装机这一次
    # 仍然生效，所以不算失败。
    if [ -d /etc/modules-load.d ]; then
        echo nf_conntrack > /etc/modules-load.d/brocade.conf 2>/dev/null \
            || echo "写不了 /etc/modules-load.d/brocade.conf，重启后连接表设置会回到内核默认。" >&2
    fi

    sysctl --system >/dev/null 2>&1 || sysctl -p "$conf" >/dev/null 2>&1 || true

    # 回读核对，跟 BBR 那边同一条理由：写进去 ≠ 生效，OpenVZ 上 sysctl 只读，写文件会成功
    # 而值纹丝不动。这里不删文件——BBR 那边删是因为文件的存在本身被控制台当作「装机时设过
    # bbr」的依据，连接表没有这层含义。
    now_max=$(cat /proc/sys/net/netfilter/nf_conntrack_max 2>/dev/null || echo '')
    if [ "$now_max" != "$want" ]; then
        echo "设了 nf_conntrack_max=$want 但当前是「${now_max:-读不到}」——多半是 OpenVZ/LXC，sysctl 只读。" >&2
        echo "不影响安装，但这台机器的连接表撑不住高并发，端口跳转会先出问题。" >&2
        return 0
    fi
    echo "连接表已调整（nf_conntrack_max=$want，UDP 超时 30/60 秒）。" >&2
}
tune_conntrack || true

if [ "$SERVICE_MODE" = "foreground" ]; then
    echo "brocade-agent enrolled node $NODE_ID (apply=$APPLY_MODE, service=foreground)"
    echo "foreground 模式不会写系统服务；当前进程会直接变成 brocade-agent run。" >&2
    # 自更新换完二进制会退出，指望 supervisor 把新的拉起来。这里没有 supervisor，所以
    # 那一步会把 agent 停在原地。preview 容器走的就是这条路，而 preview 的控制面不会
    # 批准任何 agent 发布，够不到那一步。
    echo "另外：这个模式下 agent 自更新换完就停了，没人拉它起来。" >&2
    exec env \
        BROCADE_AGENT_SERVER="$SERVER" \
        BROCADE_NODE_TOKEN_FILE="$CONFIG_DIR/token" \
        BROCADE_AGENT_STATE_DIR="$STATE_DIR" \
        BROCADE_AGENT_APPLY="$APPLY_MODE" \
        BROCADE_XRAY_BIN="$XRAY_BIN" \
        BROCADE_VPNGATE_STATS_WINDOW_SECS="$VPNGATE_STATS_WINDOW_SECS" \
        "$AGENT_BIN" run
fi

LOG_NAMESPACE_LINE=
AGENT_LOG_FILE=
if [ "$SERVICE_MODE" = "systemd" ]; then
    # Give Brocade its own journal namespace so the bootstrap 20 MiB ceiling applies to this
    # agent, not to unrelated host services. The first authenticated poll replaces it with the
    # global/per-machine value from Settings. Namespaces landed in systemd 245; on an older host
    # retaining the global journal is safer than shrinking every service's logs to our limit.
    SYSTEMD_VERSION=$(systemctl --version 2>/dev/null | awk 'NR == 1 { print $2 }')
    if [ -n "$SYSTEMD_VERSION" ] && [ "$SYSTEMD_VERSION" -ge 245 ] 2>/dev/null; then
        LOG_NAMESPACE_LINE=enabled
        install -d -m 0755 /etc/systemd/system/brocade-agent.service.d
        cat > /etc/systemd/system/brocade-agent.service.d/20-log-namespace.conf <<'EOF'
[Service]
LogNamespace=brocade-agent
EOF
        install -d -m 0755 /etc/systemd/journald@brocade-agent.conf.d
        cat > /etc/systemd/journald@brocade-agent.conf.d/limits.conf <<'EOF'
[Journal]
Storage=persistent
SystemMaxUse=20M
RuntimeMaxUse=20M
SystemMaxFileSize=5M
RuntimeMaxFileSize=5M
EOF
    else
        rm -f /etc/systemd/system/brocade-agent.service.d/20-log-namespace.conf
        echo "systemd 低于 245，不能创建独立日志命名空间；Agent 暂沿用系统 journal 上限" >&2
    fi
    cat > /etc/systemd/system/brocade-agent.service <<EOF
[Unit]
Description=Brocade agent
Wants=network-online.target
After=network-online.target

[Service]
Type=simple
EnvironmentFile=$CONFIG_DIR/env
ExecStart=$AGENT_BIN run
Restart=always
RestartSec=5
# Convergence and usage sampling are two threads in one process. A version split into two units
# existed and was wrong: step 4 samples the counters before restarting xray, that step sits in the
# middle of the convergence sequence where another process cannot insert itself, and sampling must
# also be serialized inside the agent (one in-process lock), which does not exist across processes.
#
# xray is nohup'd by the agent and stays in the same cgroup, so the default KillMode=control-group
# kills it along with the agent on exit — presenting as a console that says converged with no process
# on the machine.
KillMode=process

[Install]
WantedBy=multi-user.target
EOF
    systemctl daemon-reload
    if [ -n "$LOG_NAMESPACE_LINE" ]; then
        systemctl try-restart systemd-journald@brocade-agent.service >/dev/null 2>&1 || true
    fi
    systemctl enable brocade-agent.service
    # A resident process does not pick up a replaced binary on its own and must restart.
    systemctl restart brocade-agent.service
else
    # OpenRC's supervise-daemon supplies the same property self-update relies on as systemd's
    # Restart=always: after the resident process atomically replaces itself and exits 0, start the
    # new inode five seconds later. Do not set stopgroup: Xray and Phantun are deliberately nohup'd
    # data-plane children and must survive an Agent restart, matching KillMode=process above.
    OPENRC_RUNNER="$CONFIG_DIR/run-openrc"
    AGENT_LOG_FILE="$STATE_DIR/logs/agent.log"
    AGENT_LOG_POLICY="$STATE_DIR/log-agent-journal-max-mib"

    # EnvironmentFile is a systemd feature. The runner imports only the six names this installer
    # writes, using `export "$line"` rather than sourcing the file as shell code. Besides accepting
    # spaces and punctuation in values, this prevents a crafted --server value from becoming root
    # shell syntax when OpenRC starts the service.
    cat > "$OPENRC_RUNNER" <<EOF
#!/bin/sh
exec 2>&1
env_file="$CONFIG_DIR/env"
while IFS= read -r line || [ -n "\$line" ]; do
    case "\$line" in
        BROCADE_AGENT_SERVER=*|BROCADE_NODE_TOKEN_FILE=*|BROCADE_AGENT_STATE_DIR=*|BROCADE_AGENT_APPLY=*|BROCADE_XRAY_BIN=*|BROCADE_VPNGATE_STATS_WINDOW_SECS=*)
            export "\$line"
            ;;
    esac
done < "\$env_file"
exec "$AGENT_BIN" run
EOF
    chmod 0700 "$OPENRC_RUNNER"

    # One bounded sink receives the runner's merged stdout/stderr. It watches the same policy file
    # populated from Settings as journald does on systemd, so Alpine does not gain an unbounded log
    # merely because it has no journal namespace.
    cat > /etc/init.d/brocade-agent <<EOF
#!/sbin/openrc-run

name="Brocade agent"
description="Brocade node reconciliation agent"
supervisor="supervise-daemon"
command="$OPENRC_RUNNER"
required_files="$AGENT_BIN $CONFIG_DIR/env $CONFIG_DIR/token $OPENRC_RUNNER"
respawn_delay=5
respawn_max=0
retry="TERM/10/KILL/5"
umask=077
output_logger="$AGENT_BIN log-sink $AGENT_LOG_FILE $AGENT_LOG_POLICY"

depend() {
    need net
    use dns logger
    after firewall
}
EOF
    chmod 0755 /etc/init.d/brocade-agent
    rc-update add brocade-agent default
    # A resident process does not pick up a replaced binary on its own. `restart` also starts a
    # stopped OpenRC service.
    rc-service brocade-agent restart
    echo "OpenRC 日志：$AGENT_LOG_FILE" >&2
fi

echo "brocade-agent enrolled node $NODE_ID (apply=$APPLY_MODE, service=$SERVICE_MODE)"

if [ "$APPLY_MODE" = "state-dir" ]; then
    echo
    echo "注意：state-dir 模式只把产物写进 $STATE_DIR，不会配 wg0、不会拉起 xray。" >&2
    echo "控制台上看到的 present 只代表文件写好了。生产请用 --apply linux。" >&2
    exit 0
fi

# The self-check: wait for the first poll to come back, then look at real system state rather than
# the applied-state the agent wrote itself.
#
# What is waited for is the agent's first round, not necessarily a convergence. A machine that has
# just enrolled is usually in no released plan yet, `/agent/v1/desired` answers 204, and there is
# nothing at all for it to converge to — `health` reports that as `pending` and passes. Treating it
# as a failure meant every first install ended in a red report on a perfectly good machine.
echo
echo "自检（最多等 90 秒）..."
i=0
while [ "$i" -lt 90 ]; do
    if "$AGENT_BIN" health --state-dir "$STATE_DIR" >/dev/null 2>&1; then
        break
    fi
    i=$((i + 3))
    sleep 3
    echo "  等 agent 跑完第一轮（已等 ${i} 秒）..." >&2
done
# The verdict is the report itself, printed once here — the loop above ran quietly.
"$AGENT_BIN" health --state-dir "$STATE_DIR" || {
    # Tail the recent log directly rather than merely saying to consult journalctl — whoever just
    # installed the machine has only this terminal, and there is no reason to send them to
    # another.
    echo "自检没通过。最近的 agent 日志：" >&2
    if [ "$SERVICE_MODE" = "openrc" ]; then
        tail -n 50 "$AGENT_LOG_FILE" >&2 || true
    elif have journalctl; then
        if [ -n "$LOG_NAMESPACE_LINE" ]; then
            journalctl --namespace=brocade-agent -u brocade-agent -n 50 --no-pager >&2 || true
        else
            journalctl -u brocade-agent -n 50 --no-pager >&2 || true
        fi
    fi
    exit 1
}
