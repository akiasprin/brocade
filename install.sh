#!/bin/sh
# Install a prebuilt Brocade bundle. This script intentionally installs only the small operating
# system prerequisites; PostgreSQL remains a launcher-managed download.
set -eu
umask 077

install_root=/opt/brocade
launcher_path=$install_root/brocade-launcher
console_path=$install_root/brocade-console
command_path=/usr/local/bin/brocade
service_user=brocade
data_root=/var/lib/brocade
cache_root=/var/cache/brocade
config_root=/etc/brocade
listen_address_path=$config_root/listen-address
public_url_path=$config_root/agent-public-url
database_type_path=$config_root/database-type
database_url_path=$config_root/database-url
service_unit=/etc/systemd/system/brocade.service
socket_unit=/etc/systemd/system/brocade.socket
socket_dropin_dir=/etc/systemd/system/brocade.service.d
socket_dropin=$socket_dropin_dir/20-brocade-socket.conf

say() {
    printf '%s\n' "$*"
}

die() {
    printf 'brocade install: %s\n' "$*" >&2
    exit 1
}

usage() {
    cat <<'EOF'
Usage: [sudo] sh install.sh [--public-ip IP] [--port PORT]
                              [--managed-db | --database-url postgresql://... | --database-url-file PATH]
                              [--no-start]
       [sudo] sh install.sh --upgrade

Installs the two binaries from this release bundle, the minimal OS prerequisites, and a dedicated
non-login `brocade` account. Serves HTTP directly on the server IP, port 8080 by default.
An interactive terminal lets you confirm the detected IP, port, and database choice.
--public-ip overrides automatic local-address detection, including behind NAT.

PostgreSQL is not installed by apt/apk and no project is compiled.
Starts the managed systemd service and prints the ready URL. Without systemd, the launcher
continues in the foreground. --no-start installs files without starting the runtime.
--upgrade updates an existing installer-managed service from v0.3.1 or later. It preserves
configuration, checks the database, backs up installation/data, and runs forward migrations.
EOF
}

public_ip=
default_port=8080
port=$default_port
port_specified=false
database_type=
database_url=
database_url_file=
start_runtime=true
upgrade=false
while [ "$#" -gt 0 ]; do
    case $1 in
        -h|--help)
            usage
            exit 0
            ;;
        --no-start)
            start_runtime=false
            shift
            ;;
        --upgrade)
            upgrade=true
            shift
            ;;
        --public-ip)
            [ "$#" -ge 2 ] || die "--public-ip 后需要填写 IP 地址"
            public_ip=$2
            shift 2
            ;;
        --public-ip=*)
            public_ip=${1#--public-ip=}
            shift
            ;;
        --port)
            [ "$#" -ge 2 ] || die "--port 后需要填写端口"
            port=$2
            port_specified=true
            shift 2
            ;;
        --port=*)
            port=${1#--port=}
            port_specified=true
            shift
            ;;
        --managed-db)
            [ -z "$database_type" ] || die "--managed-db 不能与其他数据库类型同时使用"
            database_type=managed
            shift
            ;;
        --database-url-file)
            [ -z "$database_type" ] || die "--database-url-file 不能与其他数据库类型同时使用"
            [ "$#" -ge 2 ] || die "--database-url-file 后需要填写私有文件路径"
            database_type=external
            database_url_file=$2
            shift 2
            ;;
        --database-url)
            [ -z "$database_type" ] || die "--database-url 不能与其他数据库类型同时使用"
            [ "$#" -ge 2 ] || die "--database-url 后需要填写 PostgreSQL 连接地址"
            database_type=external
            database_url=$2
            shift 2
            ;;
        --database-url=*)
            [ -z "$database_type" ] || die "--database-url 不能与其他数据库类型同时使用"
            database_type=external
            database_url=${1#--database-url=}
            shift
            ;;
        *)
            usage >&2
            exit 2
            ;;
    esac
done

[ "$(id -u)" -eq 0 ] || die "需要 root 权限；请用 root 执行（有 sudo 时可执行 sudo sh install.sh）"

if [ -n "$database_url_file" ]; then
    [ -f "$database_url_file" ] && [ ! -L "$database_url_file" ] || die "数据库连接文件必须是普通文件"
    command -v stat >/dev/null 2>&1 || die "读取数据库私有文件需要 stat"
    case $(stat -c '%u:%a' "$database_url_file") in
        0:600|0:400) ;;
        *) die "数据库连接文件必须属于 root，权限为 0600 或 0400" ;;
    esac
    [ "$(stat -c '%s' "$database_url_file")" -le 2049 ] || die "数据库连接文件过大"
    database_url=$(cat "$database_url_file") || die "无法读取数据库连接文件"
fi

valid_port() {
    case $port in
        ''|*[!0-9]*) return 1 ;;
    esac
    [ "${#port}" -le 5 ] && [ "$port" -ge 1 ] && [ "$port" -le 65535 ]
}
valid_port || die "端口必须是 1 至 65535 之间的整数"

valid_database_url() {
    [ "${#database_url}" -le 2048 ] || return 1
    case $database_url in
        postgres://?*|postgresql://?*) ;;
        *) return 1 ;;
    esac
    case $database_url in
        *[[:space:]]*|*[[:cntrl:]]*) return 1 ;;
    esac
    return 0
}

# Upgrade reads the installation's authority rather than re-detecting addresses or selecting a DB.
read_upgrade_configuration() {
    [ "$start_runtime" = true ] || die "--upgrade 不能与 --no-start 同时使用"
    [ -z "$database_type" ] && [ -z "$public_ip" ] && [ "$port_specified" = false ] ||
        die "升级会保留原配置，请勿同时指定数据库、IP 或端口"
    for saved in "$listen_address_path" "$public_url_path" "$database_type_path"; do
        [ -f "$saved" ] && [ ! -L "$saved" ] || die "升级缺少原有配置：$saved"
    done
    IFS= read -r saved_listen_address < "$listen_address_path" || die "无法读取原监听地址"
    IFS= read -r saved_public_url < "$public_url_path" || die "无法读取原访问地址"
    IFS= read -r database_type < "$database_type_path" || die "无法读取原数据库类型"
    case $database_type in
        managed|managed-socket-v1) database_type=managed ;;
        external|external-socket-v1)
            database_type=external
            [ -f "$database_url_path" ] && [ ! -L "$database_url_path" ] || die "缺少原数据库连接配置"
            IFS= read -r database_url < "$database_url_path" || die "无法读取原数据库连接配置"
            valid_database_url || die "原数据库连接配置无效"
            ;;
        *) die "原数据库类型不受支持；请勿重新选择数据库来绕过"
            ;;
    esac
    [ -n "$saved_listen_address" ] && [ -n "$saved_public_url" ] || die "原入口配置为空"
    [ -f "$launcher_path" ] && [ ! -L "$launcher_path" ] &&
        [ -f "$console_path" ] && [ ! -L "$console_path" ] || die "缺少原安装二进制"
    command -v systemctl >/dev/null 2>&1 && [ -d /run/systemd/system ] ||
        die "--upgrade 当前要求安装器管理的 systemd 服务；手动部署请按文档升级"
    [ -f /etc/systemd/system/brocade.service ] &&
        [ ! -L /etc/systemd/system/brocade.service ] &&
        grep -Fq '# Managed by Brocade installer' /etc/systemd/system/brocade.service ||
        die "brocade.service 不由安装器管理，请按手动部署文档升级"
    systemctl is-active --quiet brocade.service || die "请先确认原 brocade.service 正常运行后再升级"
    command -v tar >/dev/null 2>&1 && command -v gzip >/dev/null 2>&1 ||
        die "升级压缩备份需要 tar 和 gzip"
    if [ "$database_type" = external ]; then
        command -v pg_dump >/dev/null 2>&1 && command -v pg_restore >/dev/null 2>&1 ||
            die "外部数据库升级需先安装匹配服务端版本的 pg_dump、pg_restore 客户端"
    fi
}
if [ "$upgrade" = true ]; then
    read_upgrade_configuration
fi

# Validate ownership before publishing any files, including a fresh install over an old layout.
if [ "$start_runtime" = true ]; then
    for owned_unit in "$service_unit" "$socket_unit" "$socket_dropin"; do
        if [ -e "$owned_unit" ] || [ -L "$owned_unit" ]; then
            [ -f "$owned_unit" ] && [ ! -L "$owned_unit" ] &&
                grep -Fq '# Managed by Brocade installer' "$owned_unit" ||
                die "$owned_unit 不由安装器管理；请先按手动部署流程迁移"
        fi
    done
    [ ! -L "$socket_dropin_dir" ] || die "$socket_dropin_dir 不能是符号链接"
fi

prompt_database_type() {
    [ -t 0 ] || die "非交互安装必须指定 --managed-db 或 --database-url postgresql://..."
    while :; do
        say "请选择数据库方式："
        say "  1) Brocade 托管 PostgreSQL（推荐；首次启动自动下载并初始化固定版本）"
        say "  2) 已有 PostgreSQL（需要填写连接地址；连接失败不会回退到新建空库）"
        printf '请输入 1 或 2：'
        IFS= read -r choice || die "未读取到数据库类型"
        case $choice in
            1)
                database_type=managed
                return
                ;;
            2)
                database_type=external
                return
                ;;
            *) say "选择无效，请重新输入。" ;;
        esac
    done
}

[ -n "$database_type" ] || prompt_database_type
if [ "$database_type" = external ]; then
    while :; do
        if [ -z "$database_url" ]; then
            [ -t 0 ] || die "--database-url 后需要填写 PostgreSQL 连接地址"
            printf '请输入 PostgreSQL 连接地址（postgresql://...）：'
            terminal_state=$(stty -g) || die "无法读取终端状态"
            trap 'stty "$terminal_state"; exit 130' HUP INT TERM
            stty -echo
            if IFS= read -r database_url; then
                database_read=true
            else
                database_read=false
            fi
            stty "$terminal_state"
            trap - HUP INT TERM
            printf '\n'
            [ "$database_read" = true ] || die "未读取到 PostgreSQL 连接地址"
        fi
        if valid_database_url; then
            break
        fi
        [ -t 0 ] || die "PostgreSQL 连接地址无效；必须使用 postgres:// 或 postgresql://，且不能包含空白"
        say "PostgreSQL 连接地址无效；必须使用 postgres:// 或 postgresql://，且不能包含空白。"
        database_url=
    done
fi

say "安装配置："
say "  公网接入：服务器 IP 直连 HTTP（默认端口 8080）"
if [ "$database_type" = managed ]; then
    say "  数据库：Brocade 托管 PostgreSQL"
else
    say "  数据库：已有 PostgreSQL"
fi

script_dir=$(CDPATH='' cd -P "$(dirname "$0")" && pwd)
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
# musl bundles are static; their launcher selects PostgreSQL for the host libc at runtime.
case "$bundle_platform" in
    "$machine_arch-$machine_libc"|"$machine_arch-musl") ;;
    *) die "发行包是 $bundle_platform，当前系统是 $machine_arch-$machine_libc" ;;
esac

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
for managed_path in "$install_root" "$data_root" "$cache_root" "$config_root"; do
    [ ! -L "$managed_path" ] || die "$managed_path 不能是符号链接"
done
install -d -o root -g root -m 0755 "$install_root"
[ -d /usr/local/bin ] || install -d -o root -g root -m 0755 /usr/local/bin
install -d -o "$service_user" -g "$service_group" -m 0700 "$data_root" "$cache_root"
install -d -o root -g root -m 0755 "$config_root"
for config_path in \
    "$listen_address_path" "$public_url_path" \
    "$database_type_path" "$database_url_path"; do
    if [ -e "$config_path" ] || [ -L "$config_path" ]; then
        [ -f "$config_path" ] && [ ! -L "$config_path" ] ||
            die "$config_path 已存在但不是普通文件"
    fi
done

launcher_stage=$install_root/.brocade-launcher.$$
console_stage=$install_root/.brocade-console.$$
command_stage=/usr/local/bin/.brocade.$$
listen_address_stage=$config_root/.listen-address.$$
public_url_stage=$config_root/.agent-public-url.$$
database_type_stage=$config_root/.database-type.$$
database_url_stage=$config_root/.database-url.$$
service_stage=$config_root/.brocade.service.$$
socket_stage=$config_root/.brocade.socket.$$
socket_dropin_stage=$config_root/.brocade-socket-dropin.$$
ready_error=$config_root/.install-ready.$$
upgrade_service_stopped=false
upgrade_published=false
upgrade_success=false
upgrade_backup=
socket_existed=false
socket_dropin_existed=false
socket_was_active=false
[ ! -f "$socket_unit" ] || socket_existed=true
[ ! -f "$socket_dropin" ] || socket_dropin_existed=true
if [ "$socket_existed" = true ] && command -v systemctl >/dev/null 2>&1 &&
    systemctl is-active --quiet brocade.socket; then
    socket_was_active=true
fi
stop_runtime() {
    # A live socket would otherwise reactivate Console in the middle of the cold backup.
    if [ -f "$socket_unit" ]; then systemctl stop brocade.socket; fi
    systemctl stop brocade.service
}
cleanup_staging() {
    if [ "$upgrade_service_stopped" = true ] && [ "$upgrade_success" = false ]; then
        if [ -e "$upgrade_backup/database/migration-started" ]; then
            stop_runtime || true
            say "升级未完成，数据库迁移可能已开始；服务保持停止，不自动回退旧程序或恢复数据库。" >&2
            say "备份与恢复记录：$upgrade_backup" >&2
        else
            if [ "$upgrade_published" = true ]; then
                stop_runtime || true
                if ! tar -C / -xzpf "$upgrade_backup/installation.tar.gz"; then
                    say "恢复旧安装失败；服务保持停止，请检查 $upgrade_backup" >&2
                    return
                fi
                if [ "$socket_existed" = false ]; then
                    systemctl disable brocade.socket || true
                    rm -f "$socket_unit"
                fi
                if [ "$socket_dropin_existed" = false ]; then rm -f "$socket_dropin"; fi
                systemctl daemon-reload || return
            fi
            # Explicit recovery must not inherit the failed candidate's systemd start counter.
            systemctl reset-failed brocade.service || say "清理旧服务启动限制失败，请检查 $upgrade_backup" >&2
            if [ "$socket_was_active" = true ]; then
                systemctl reset-failed brocade.socket || true
                systemctl start brocade.socket || say "旧监听恢复失败，请检查 $upgrade_backup" >&2
            fi
            systemctl start brocade.service || say "旧服务恢复失败，请检查 $upgrade_backup" >&2
        fi
    fi
    rm -f "$launcher_stage" "$console_stage" "$command_stage" \
        "$listen_address_stage" "$public_url_stage" \
        "$database_type_stage" "$database_url_stage" "$service_stage" \
        "$socket_stage" "$socket_dropin_stage" "$ready_error"
}
trap cleanup_staging 0
trap 'exit 130' HUP INT TERM

install -o root -g root -m 0755 "$script_dir/brocade-launcher" "$launcher_stage"
install -o root -g root -m 0755 "$script_dir/brocade-console" "$console_stage"

# Run verified bundle bytes as the dedicated account, including address detection.
run_launcher() {
    if command -v su-exec >/dev/null 2>&1; then
        su-exec "$service_user" "$launcher_stage" "$@"
    else
        setpriv --reuid="$(id -u "$service_user")" --regid="$(id -g "$service_user")" \
            --init-groups "$launcher_stage" "$@"
    fi
}
run_launcher --version >/dev/null || die "launcher 无法在当前系统执行；请检查上方的动态加载器或运行库错误"
resolve_origin() {
    set -- address --port "$port"
    [ -z "$public_ip" ] || set -- "$@" --public-ip "$public_ip"
    run_launcher "$@"
}

if [ "$upgrade" = true ]; then
    public_url=$saved_public_url
    listen_address=$saved_listen_address
else
if [ -z "$public_ip" ] && [ -t 0 ]; then
    detected_origin=$(resolve_origin) || die "无法识别 IP；请加 --public-ip IP 重试"
    detected_ip=${detected_origin#http://}
    detected_ip=${detected_ip%:*}
    detected_ip=${detected_ip#\[}
    detected_ip=${detected_ip%\]}
    printf '服务器访问 IP [%s]：' "$detected_ip"
    IFS= read -r public_ip || die "未读取到服务器 IP"
    public_ip=${public_ip:-$detected_ip}
fi
if [ "$port_specified" = false ] && [ -t 0 ]; then
    while :; do
        printf 'HTTP 端口 [%s]：' "$default_port"
        IFS= read -r selected_port || die "未读取到 HTTP 端口"
        port=${selected_port:-$default_port}
        valid_port && break
        say "端口必须是 1 至 65535 之间的整数。"
    done
fi
public_url=$(resolve_origin) || die "IP 地址无效；请通过 --public-ip IP 指定服务器地址"
case $public_url in
    http://\[*) listen_address="[::]:$port" ;;
    *) listen_address="0.0.0.0:$port" ;;
esac
fi

run_upgrade_database() {
    if [ "$database_type" = external ]; then
        (
            DATABASE_URL=$database_url
            export DATABASE_URL
            run_launcher upgrade-database --external-db --previous-launcher "$launcher_path" \
                --console-bin "$console_stage" --backup-dir "$upgrade_backup/database" \
                --data-dir "$data_root" --cache-dir "$cache_root" "$@"
        )
    else
        (
            unset DATABASE_URL
            run_launcher upgrade-database --managed-db --previous-launcher "$launcher_path" \
                --console-bin "$console_stage" --backup-dir "$upgrade_backup/database" \
                --data-dir "$data_root" --cache-dir "$cache_root" \
                --cold-backup "$upgrade_backup/data.tar.gz" "$@"
        )
    fi
}

if [ "$upgrade" = true ]; then
    backup_root=/var/backups/brocade-upgrades
    [ ! -L "$backup_root" ] || die "升级备份目录不能是符号链接"
    install -d -o root -g "$service_group" -m 0710 "$backup_root"
    upgrade_backup=$backup_root/$(date -u +%Y%m%dT%H%M%SZ)-$$
    mkdir -m 0710 "$upgrade_backup"
    chown root:"$service_group" "$upgrade_backup"
    install -d -o "$service_user" -g "$service_group" -m 0700 "$upgrade_backup/database"
    say "检查 v0.3.1 起的数据库兼容性…"
    run_upgrade_database --check-only
    say "停止原服务并备份安装、配置、密钥和数据…"
    upgrade_service_stopped=true
    stop_runtime
    set -- \
        opt/brocade/brocade-launcher opt/brocade/brocade-console \
        opt/brocade/BROCADE_PLATFORM opt/brocade/SHA256SUMS \
        usr/local/bin/brocade etc/brocade etc/systemd/system/brocade.service
    if [ -d /etc/systemd/system/brocade.service.d ]; then
        set -- "$@" etc/systemd/system/brocade.service.d
    fi
    if [ -f "$socket_unit" ]; then set -- "$@" etc/systemd/system/brocade.socket; fi
    if [ -d /etc/systemd/system/brocade.socket.d ]; then
        set -- "$@" etc/systemd/system/brocade.socket.d
    fi
    tar -C / -czpf "$upgrade_backup/installation.tar.gz" "$@"
    tar -C /var/lib -czpf "$upgrade_backup/data.tar.gz" brocade
    chown root:"$service_group" "$upgrade_backup/data.tar.gz"
    chmod 0640 "$upgrade_backup/data.tar.gz"
    sha256sum "$launcher_path" "$console_path" > "$upgrade_backup/previous-binaries.sha256"
    say "备份数据库并执行前向迁移…"
    run_upgrade_database
fi

# The public command supplies system paths and, when invoked through sudo/root, drops privileges
# before the Rust launcher starts. This keeps `sudo brocade up` convenient without ever running
# initdb or Console as root.
cat >"$command_stage" <<'EOF'
#!/bin/sh
set -eu
service_user=brocade
runtime=/opt/brocade/brocade-launcher
listen_address_path=/etc/brocade/listen-address
public_url_path=/etc/brocade/agent-public-url
database_type_path=/etc/brocade/database-type
database_url_path=/etc/brocade/database-url
: "${BROCADE_DATA_DIR:=/var/lib/brocade}"
: "${BROCADE_CACHE_DIR:=/var/cache/brocade}"
export BROCADE_DATA_DIR BROCADE_CACHE_DIR

if [ -z "${BROCADE_AGENT_PUBLIC_URL:-}" ] && [ -r "$public_url_path" ]; then
    IFS= read -r BROCADE_AGENT_PUBLIC_URL < "$public_url_path" || {
        printf 'brocade: 无法读取访问地址配置\n' >&2
        exit 1
    }
    export BROCADE_AGENT_PUBLIC_URL
fi
if [ -z "${BROCADE_ADMIN_BIND:-}" ] && [ -r "$listen_address_path" ]; then
    IFS= read -r BROCADE_ADMIN_BIND < "$listen_address_path" || {
        printf 'brocade: 无法读取监听地址配置\n' >&2
        exit 1
    }
    export BROCADE_ADMIN_BIND
fi

database_type=
if [ -r "$database_type_path" ]; then
    IFS= read -r database_type < "$database_type_path" || {
        printf 'brocade: 无法读取数据库类型配置\n' >&2
        exit 1
    }
fi
case $database_type in
    managed|managed-socket-v1) database_type=managed ;;
    external|external-socket-v1)
        database_type=external
        if [ -z "${DATABASE_URL:-}" ]; then
            [ -r "$database_url_path" ] || {
                printf 'brocade: 外部数据库模式缺少连接地址配置\n' >&2
                exit 1
            }
            IFS= read -r DATABASE_URL < "$database_url_path" || {
                printf 'brocade: 无法读取数据库连接地址配置\n' >&2
                exit 1
            }
            case $DATABASE_URL in
                postgres://?*|postgresql://?*) ;;
                *)
                    printf 'brocade: 数据库连接地址必须使用 postgres:// 或 postgresql://\n' >&2
                    exit 1
                    ;;
            esac
            export DATABASE_URL
        fi
        ;;
    '') ;;
    *)
        printf 'brocade: 未识别的数据库类型配置：%s\n' "$database_type" >&2
        exit 1
        ;;
esac

if [ "${1-}" = up ] && { [ "$database_type" = managed ] || [ "$database_type" = external ]; }; then
    database_requested=false
    for argument in "$@"; do
        case $argument in
            --managed-db|--external-db) database_requested=true ;;
        esac
    done
    if [ "$database_requested" = false ]; then
        if [ "$database_type" = managed ]; then
            set -- "$@" --managed-db
        else
            set -- "$@" --external-db
        fi
    fi
fi

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

printf '%s\n' "$listen_address" >"$listen_address_stage"
chmod 0644 "$listen_address_stage"
printf '%s\n' "$public_url" >"$public_url_stage"
chmod 0644 "$public_url_stage"
# This tagged selector is also the installation-format boundary. Old installers only
# recognize managed/external and must reject this layout before stopping its live socket.
printf '%s-socket-v1\n' "$database_type" >"$database_type_stage"
chmod 0644 "$database_type_stage"
if [ "$database_type" = external ]; then
    printf '%s\n' "$database_url" >"$database_url_stage"
    chown root:"$service_group" "$database_url_stage"
    chmod 0640 "$database_url_stage"
fi

# Publish Console and launcher before their defaults, then publish the public command last. A
# concurrent invocation therefore uses either the previous wrapper or a complete new configuration.
if [ "$upgrade" = true ]; then upgrade_published=true; fi
mv -f "$console_stage" "$console_path"
mv -f "$launcher_stage" "$launcher_path"
install -o root -g root -m 0644 "$script_dir/BROCADE_PLATFORM" "$install_root/BROCADE_PLATFORM"
install -o root -g root -m 0644 "$script_dir/SHA256SUMS" "$install_root/SHA256SUMS"
mv -f "$public_url_stage" "$public_url_path"
mv -f "$listen_address_stage" "$listen_address_path"
# One-time migration: the new wrapper no longer reads the retired access-mode selector.
rm -f "$config_root/installation-type"
if [ "$database_type" = external ]; then
    mv -f "$database_url_stage" "$database_url_path"
    mv -f "$database_type_stage" "$database_type_path"
else
    mv -f "$database_type_stage" "$database_type_path"
    rm -f "$database_url_path"
fi
mv -f "$command_stage" "$command_path"
if [ "$upgrade" = false ]; then trap - 0 HUP INT TERM; fi

say "Brocade 已安装完成。"
say "访问方式：服务器 IP 直连 HTTP"
say "控制台地址：$public_url"
say "监听地址：$listen_address"
if [ "$database_type" = managed ]; then
    say "数据库：Brocade 托管 PostgreSQL（首次启动自动下载并初始化）"
else
    say "数据库：已有 PostgreSQL（连接地址已保存到仅 root 与 brocade 账号可读的配置）"
fi
if [ "$start_runtime" = false ]; then
    say "已按 --no-start 跳过启动；运行 sudo brocade up 后打开输出的完整初始化链接。"
    exit 0
fi

if ! command -v systemctl >/dev/null 2>&1 || [ ! -d /run/systemd/system ]; then
    say "当前系统没有 systemd，正在前台启动 Brocade；请保持此终端运行。"
    exec "$command_path" up
fi

if [ -e "$service_unit" ] || [ -L "$service_unit" ]; then
    [ -f "$service_unit" ] && [ ! -L "$service_unit" ] &&
        grep -Fq '# Managed by Brocade installer' "$service_unit" ||
        die "brocade.service 已存在且不由安装器管理；请核对服务配置后使用 --no-start 安装"
fi
if [ "$upgrade" = false ]; then
cat >"$service_stage" <<EOF
# Managed by Brocade installer
[Unit]
Description=Brocade Console and managed runtime
Wants=network-online.target
After=network-online.target
StartLimitIntervalSec=300
StartLimitBurst=3

[Service]
Type=simple
User=$service_user
Group=$service_group
ExecStart=$command_path up
Restart=on-failure
RestartSec=5s
KillMode=mixed
TimeoutStopSec=60s

[Install]
WantedBy=multi-user.target
EOF
chmod 0644 "$service_stage"
mv -f "$service_stage" "$service_unit"
fi

# Keep the listener in PID 1 across ordinary service restarts. Full --upgrade deliberately
# stops both units while backing up/migrating, then restores socket activation afterwards.
if [ "$upgrade" = false ] && [ "$socket_was_active" = true ]; then stop_runtime; fi
cat >"$socket_stage" <<EOF
# Managed by Brocade installer
[Unit]
Description=Brocade Console listener

[Socket]
ListenStream=$listen_address
Accept=no
Backlog=4096
NoDelay=true
FileDescriptorName=admin
Service=brocade.service

[Install]
WantedBy=sockets.target
EOF
cat >"$socket_dropin_stage" <<'EOF'
# Managed by Brocade installer
[Unit]
Requires=brocade.socket
After=brocade.socket
EOF
chmod 0644 "$socket_stage" "$socket_dropin_stage"
install -d -o root -g root -m 0755 "$socket_dropin_dir"
mv -f "$socket_stage" "$socket_unit"
mv -f "$socket_dropin_stage" "$socket_dropin"
systemctl daemon-reload
systemctl enable brocade.service brocade.socket
if [ "$upgrade" = true ]; then
    # Repeated operator-requested upgrades may exhaust the old unit's StartLimitBurst even when
    # every previous process was healthy. Reset once; the unit's normal retry limits still apply.
    systemctl reset-failed brocade.service
fi
systemctl reset-failed brocade.socket
systemctl start brocade.socket
if [ "$upgrade" = true ]; then
    systemctl start brocade.service
else
    systemctl restart brocade.service
fi

say "正在等待数据库、控制台与公网入口就绪…"
if [ "$upgrade" = false ]; then trap 'rm -f "$ready_error"' 0; fi
ready_attempt=0
until "$command_path" url >/dev/null 2>"$ready_error"; do
    ready_attempt=$((ready_attempt + 1))
    if [ "$ready_attempt" -ge 600 ] || systemctl is-failed --quiet brocade.service; then
        cat "$ready_error" >&2
        die "Brocade 未能就绪；请运行 systemctl status brocade.service 和 journalctl -u brocade.service 检查"
    fi
    sleep 1
done
rm -f "$ready_error"
upgrade_success=true
trap - 0 HUP INT TERM
if [ "$upgrade" = true ]; then say "升级完成；备份保留在 $upgrade_backup"; fi
say "Brocade 已启动并设置开机启动。请打开下面的完整链接；首次使用时创建管理员："
"$command_path" url
