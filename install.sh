#!/bin/sh
# Install a prebuilt Brocade bundle. This script intentionally installs only the small operating
# system prerequisites; PostgreSQL and cloudflared remain launcher-managed downloads.
set -eu
umask 077

install_root=/opt/brocade
launcher_path=$install_root/brocade-launcher
console_path=$install_root/brocade-console
command_path=/usr/local/bin/brocade
service_user=brocade
data_root=/var/lib/brocade
cache_root=/var/cache/brocade

say() {
    printf '%s\n' "$*"
}

die() {
    printf 'brocade install: %s\n' "$*" >&2
    exit 1
}

usage() {
    cat <<'EOF'
Usage: [sudo] sh install.sh

Installs the two binaries from this release bundle, the minimal OS prerequisites, and a dedicated
non-login `brocade` account. PostgreSQL is not installed by apt/apk and no project is compiled.
EOF
}

case ${1-} in
    -h|--help)
        usage
        exit 0
        ;;
    '') ;;
    *)
        usage >&2
        exit 2
        ;;
esac

[ "$(id -u)" -eq 0 ] || die "需要 root 权限；请用 root 执行（有 sudo 时可执行 sudo sh install.sh）"

script_dir=$(CDPATH= cd -P "$(dirname "$0")" && pwd)
for required in brocade-launcher brocade-console BROCADE_PLATFORM SHA256SUMS; do
    [ -f "$script_dir/$required" ] || die "发行包缺少 $required"
done
command -v sha256sum >/dev/null 2>&1 || die "系统缺少 sha256sum，无法先验证发行包"

# Verify before installing packages or touching system paths. The manifest is generated beside the
# binaries by CI and names only files in this bundle.
say "验证发行包…"
(cd "$script_dir" && sha256sum -c SHA256SUMS) || die "发行包 SHA-256 校验失败"

case $(uname -m) in
    x86_64|amd64) machine_arch=x86_64 ;;
    aarch64|arm64) machine_arch=aarch64 ;;
    *) die "只支持 x86_64 与 aarch64，当前为 $(uname -m)" ;;
esac

ldd_banner=$(ldd --version 2>&1 || true)
case "$ldd_banner" in
    *musl*) machine_libc=musl ;;
    *GLIBC*|*glibc*|*GNU*) machine_libc=gnu ;;
    *)
        if [ -f /etc/alpine-release ]; then
            machine_libc=musl
        else
            die "无法判断当前系统使用 GNU libc 还是 musl"
        fi
        ;;
esac

bundle_platform=$(sed -n '1p' "$script_dir/BROCADE_PLATFORM")
[ "$bundle_platform" = "$machine_arch-$machine_libc" ] ||
    die "发行包是 $bundle_platform，当前系统是 $machine_arch-$machine_libc"

has_ca_bundle() {
    for candidate in \
        /etc/ssl/certs/ca-certificates.crt \
        /etc/pki/tls/certs/ca-bundle.crt \
        /etc/ssl/ca-bundle.pem; do
        [ -s "$candidate" ] && return 0
    done
    return 1
}

has_privilege_dropper() {
    command -v su-exec >/dev/null 2>&1 && return 0
    command -v setpriv >/dev/null 2>&1 && setpriv --help 2>&1 | grep -q 'reuid' && return 0
    return 1
}

has_gnu_runtime_libraries() {
    [ "$machine_libc" = musl ] && return 0
    found_liblzma=false
    found_libgcc=false
    for candidate in \
        /lib/liblzma.so.5 /lib/*/liblzma.so.5 /lib64/liblzma.so.5 \
        /usr/lib/liblzma.so.5 /usr/lib/*/liblzma.so.5 /usr/lib64/liblzma.so.5; do
        [ -e "$candidate" ] && found_liblzma=true
    done
    for candidate in \
        /lib/libgcc_s.so.1 /lib/*/libgcc_s.so.1 /lib64/libgcc_s.so.1 \
        /usr/lib/libgcc_s.so.1 /usr/lib/*/libgcc_s.so.1 /usr/lib64/libgcc_s.so.1; do
        [ -e "$candidate" ] && found_libgcc=true
    done
    [ "$found_liblzma" = true ] && [ "$found_libgcc" = true ]
}

install_prerequisites() {
    say "安装系统 CA 与服务账号工具…"
    if command -v apt-get >/dev/null 2>&1; then
        export DEBIAN_FRONTEND=noninteractive
        apt-get update
        apt-get install -y --no-install-recommends \
            ca-certificates passwd util-linux liblzma5 libgcc-s1
    elif command -v apk >/dev/null 2>&1; then
        apk add --no-cache ca-certificates su-exec
    elif command -v dnf >/dev/null 2>&1; then
        dnf install -y ca-certificates shadow-utils util-linux xz-libs libgcc
    elif command -v yum >/dev/null 2>&1; then
        yum install -y ca-certificates shadow-utils util-linux xz-libs libgcc
    elif command -v zypper >/dev/null 2>&1; then
        zypper --non-interactive install ca-certificates shadow util-linux liblzma5 libgcc_s1
    else
        die "无法自动安装 ca-certificates 与账号工具：不支持当前包管理器"
    fi
}

if ! has_ca_bundle || ! has_privilege_dropper || ! has_gnu_runtime_libraries ||
    { ! command -v useradd >/dev/null 2>&1 && ! command -v adduser >/dev/null 2>&1; }; then
    install_prerequisites
fi
command -v update-ca-certificates >/dev/null 2>&1 && update-ca-certificates >/dev/null
has_ca_bundle || die "ca-certificates 安装后仍找不到系统 CA bundle"
has_privilege_dropper || die "安装后仍缺少可用的 setpriv 或 su-exec"
has_gnu_runtime_libraries || die "安装后仍缺少 launcher 所需的 liblzma 或 libgcc"

if id "$service_user" >/dev/null 2>&1; then
    [ "$(id -u "$service_user")" -ne 0 ] || die "现有 brocade 账号不能是 root"
else
    nologin=/bin/false
    [ -x /usr/sbin/nologin ] && nologin=/usr/sbin/nologin
    [ -x /sbin/nologin ] && nologin=/sbin/nologin
    if command -v useradd >/dev/null 2>&1; then
        useradd --system --user-group --no-create-home \
            --home-dir "$data_root" --shell "$nologin" "$service_user"
    elif command -v adduser >/dev/null 2>&1; then
        if ! grep -q "^${service_user}:" /etc/group; then
            addgroup -S "$service_user"
        fi
        adduser -S -D -H -h "$data_root" -s "$nologin" -G "$service_user" "$service_user"
    else
        die "无法创建 brocade 服务账号"
    fi
fi

service_group=$(id -gn "$service_user")
for managed_path in "$install_root" "$data_root" "$cache_root"; do
    [ ! -L "$managed_path" ] || die "$managed_path 不能是符号链接"
done
install -d -o root -g root -m 0755 "$install_root"
[ -d /usr/local/bin ] || install -d -o root -g root -m 0755 /usr/local/bin
install -d -o "$service_user" -g "$service_group" -m 0700 "$data_root" "$cache_root"

launcher_stage=$install_root/.brocade-launcher.$$
console_stage=$install_root/.brocade-console.$$
command_stage=/usr/local/bin/.brocade.$$
cleanup_staging() {
    rm -f "$launcher_stage" "$console_stage" "$command_stage"
}
trap cleanup_staging 0 HUP INT TERM

install -o root -g root -m 0755 "$script_dir/brocade-launcher" "$launcher_stage"
install -o root -g root -m 0755 "$script_dir/brocade-console" "$console_stage"

# Compatibility-check untrusted executable bytes only after dropping to the dedicated account.
if command -v su-exec >/dev/null 2>&1; then
    su-exec "$service_user" "$launcher_stage" --version >/dev/null 2>&1 ||
        die "launcher 无法在当前系统执行"
else
    service_uid=$(id -u "$service_user")
    service_gid=$(id -g "$service_user")
    setpriv --reuid="$service_uid" --regid="$service_gid" --init-groups \
        "$launcher_stage" --version >/dev/null 2>&1 || die "launcher 无法在当前系统执行"
fi

# The public command supplies system paths and, when invoked through sudo/root, drops privileges
# before the Rust launcher starts. This keeps `sudo brocade up` convenient without ever running
# initdb or Console as root.
cat >"$command_stage" <<'EOF'
#!/bin/sh
set -eu
service_user=brocade
runtime=/opt/brocade/brocade-launcher
: "${BROCADE_DATA_DIR:=/var/lib/brocade}"
: "${BROCADE_CACHE_DIR:=/var/cache/brocade}"
export BROCADE_DATA_DIR BROCADE_CACHE_DIR

if [ "$(id -u)" -eq 0 ]; then
    HOME=/var/lib/brocade
    export HOME
    if command -v su-exec >/dev/null 2>&1; then
        exec su-exec "$service_user" "$runtime" "$@"
    fi
    if command -v setpriv >/dev/null 2>&1 && setpriv --help 2>&1 | grep -q 'reuid'; then
        uid=$(id -u "$service_user")
        gid=$(id -g "$service_user")
        exec setpriv --reuid="$uid" --regid="$gid" --init-groups "$runtime" "$@"
    fi
    printf 'brocade: 无法切换到 brocade 服务账号\n' >&2
    exit 1
fi

if [ "$(id -u)" -ne "$(id -u "$service_user")" ]; then
    printf 'brocade: 系统安装版只能由 root 或 brocade 服务账号启动\n' >&2
    exit 1
fi
HOME=/var/lib/brocade
export HOME
exec "$runtime" "$@"
EOF
chmod 0755 "$command_stage"

# Publish Console first and launcher last, so a concurrent new invocation never sees a launcher
# newer than its required sibling.
mv -f "$console_stage" "$console_path"
mv -f "$launcher_stage" "$launcher_path"
mv -f "$command_stage" "$command_path"
install -o root -g root -m 0644 "$script_dir/BROCADE_PLATFORM" "$install_root/BROCADE_PLATFORM"
install -o root -g root -m 0644 "$script_dir/SHA256SUMS" "$install_root/SHA256SUMS"
trap - 0 HUP INT TERM

say "Brocade 已安装完成。"
say "普通启动：brocade up（非 root shell 使用 sudo brocade up）"
say "临时 Tunnel：brocade up --tunnel（非 root shell 使用 sudo）"
