#!/usr/bin/env bash
# Run on the server whose public IP you want Google to associate with Hong Kong.
# HTTP success only confirms delivery; these requests cannot guarantee GeoIP changes.
set -euo pipefail
umask 077

if [[ "${1:-}" == "--help" ]]; then
  cat <<'HELP'
Usage: bash google-hk.sh
       IP_FAMILY=6 bash google-hk.sh

Send one round of Hong Kong search, map and news requests, 45-75 seconds apart.
Requires Bash, curl and flock. Defaults to IPv4 and connects directly from this
server, ignoring proxy environment variables. Run as your ordinary user.
Cookies are saved under ${XDG_STATE_HOME:-$HOME/.local/state}/google-hk.
Google may ignore these location signals; HTTP 200 does not prove a GeoIP change.
HELP
  exit 0
fi

if (( $# != 0 )); then
  printf 'Unknown argument; use --help.\n' >&2
  exit 2
fi

family="${IP_FAMILY:-4}"
case "$family" in
  4|6) ;;
  *) printf 'IP_FAMILY must be 4 or 6.\n' >&2; exit 2 ;;
esac

for dependency in curl flock; do
  command -v "$dependency" >/dev/null || {
    printf 'Missing dependency: %s\n' "$dependency" >&2
    exit 1
  }
done

state_dir="${XDG_STATE_HOME:-${HOME:?HOME is required}/.local/state}/google-hk"
mkdir -p -- "$state_dir"
exec 9>"$state_dir/ipv${family}.lock"
if ! flock -n 9; then
  printf 'Another IPv%s round is running; skipped.\n' "$family"
  exit 0
fi

cookie_file="$state_dir/cookies-ipv${family}.txt"
touch -- "$cookie_file"
chmod 600 -- "$cookie_file"
user_agent='Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36'

request() {
  local label="$1" url="$2" status
  shift 2
  printf '%s: sending via IPv%s...\n' "$label" "$family"
  if status=$(curl -q "-$family" --noproxy '*' \
    --silent --show-error --fail --location --compressed \
    --proto '=https' --proto-redir '=https' \
    --connect-timeout 10 --max-time 30 \
    --cookie "$cookie_file" --cookie-jar "$cookie_file" \
    --user-agent "$user_agent" --header 'Accept-Language: zh-HK,zh-TW;q=0.9,en;q=0.8' \
    --get --data-urlencode 'hl=zh-HK' --data-urlencode 'gl=HK' \
    --output /dev/null --write-out '%{http_code}' "$@" "$url"); then
    printf '%s: HTTP %s (location change unverified).\n' "$label" "$status"
  else
    printf '%s: request failed (HTTP %s); stopping this round.\n' "$label" "$status" >&2
    return 1
  fi
}

pause() {
  local seconds=$((45 + RANDOM % 31))
  printf 'Waiting %s seconds...\n' "$seconds"
  sleep "$seconds"
}

keywords=('香港 天氣' '香港 新聞' '香港 咖啡店' '港鐵 路線' '香港 餐廳')
keyword="${keywords[RANDOM % ${#keywords[@]}]}"
request 'Search' 'https://www.google.com/search' --data-urlencode "q=$keyword"
pause
# Coordinates select the map view; they do not report the device's GPS position.
request 'Maps' 'https://www.google.com/maps/search/cafe/@22.2847,114.1582,16z'
pause
request 'News' 'https://news.google.com/home' --data-urlencode 'ceid=HK:zh-Hant'
printf 'Round complete. Google IP geolocation remains unverified.\n'
