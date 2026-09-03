#!/bin/sh
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
xray_root="$repo_root/components/xray-core"
asset_dir="${XRAY_ASSET_DIR:-$xray_root/resources}"
cache_dir="${XRAY_ASSET_CACHE_DIR:-$repo_root/.tools}"
geodata_base="${BROCADE_GEODATA_BASE:-https://raw.githubusercontent.com/Loyalsoldier/v2ray-rules-dat/release}"

# These are the assets currently used by the v26.4.25 fork checkout. Change them only together
# with an intentional rule-data refresh; the files themselves stay ignored by the Xray fork.
geoip_sha256=0d5d2ba0c5a5c58027fd1347a6afd57c9470799b6bb3cbc274fd4657ed8de382
geosite_sha256=7774ebc22c0a5acc718a4ee8635a96475021656937725551945d9a446591bf54

mkdir -p "$asset_dir"

fetch_asset() {
    name=$1
    expected_sha256=$2
    destination="$asset_dir/$name"
    cached="$cache_dir/$name"
    temporary="$destination.tmp.$$"

    # Caches outlive the pinned release. Treat an old cache or destination as a miss so an
    # intentional checksum refresh can download the new asset instead of failing on stale bytes.
    copy_if_matches() {
        source=$1
        [ -s "$source" ] || return 1
        actual_sha256=$(sha256sum "$source" | awk '{print $1}') || return 1
        [ "$actual_sha256" = "$expected_sha256" ] || return 1
        cp -f "$source" "$temporary"
    }

    if ! copy_if_matches "$cached" && ! copy_if_matches "$destination"; then
        command -v curl >/dev/null 2>&1 || {
            echo "curl is required to download $name" >&2
            exit 1
        }
        curl -fsSL --retry 3 --max-time 120 "$geodata_base/$name" -o "$temporary"
    fi

    [ -s "$temporary" ] || {
        rm -f "$temporary"
        echo "downloaded $name is empty" >&2
        exit 1
    }

    actual_sha256=$(sha256sum "$temporary" | awk '{print $1}')
    if [ "$actual_sha256" != "$expected_sha256" ]; then
        rm -f "$temporary"
        echo "$name checksum mismatch: expected $expected_sha256, got $actual_sha256" >&2
        echo "refresh the pinned checksum only as an intentional rule-data change" >&2
        exit 1
    fi

    mv -f "$temporary" "$destination"
}

fetch_asset geoip.dat "$geoip_sha256"
fetch_asset geosite.dat "$geosite_sha256"

printf 'prepared Xray assets in %s\n' "$asset_dir"
sha256sum "$asset_dir/geoip.dat" "$asset_dir/geosite.dat"
