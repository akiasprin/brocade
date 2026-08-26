#!/bin/bash
# Start a brocade node. Deliberately not through wg-quick up but split into explicit steps — the
# review's point that "in:backbone binds the overlay, so xray may fail to bind before wg0" is
# reproduced by controlling their order, and wg-quick hides it.
set -euo pipefail

NODE="${NODE_ID:?要设 NODE_ID}"
WG_CONF="/etc/wireguard/${NODE}.wg0.conf"
XRAY_CONF="/etc/xray/${NODE}.xray.json"
: "${START_ORDER:=wg-first}"     # wg-first | xray-first | agent-idle
: "${WG_ENABLED:=1}"

bring_up_wg(){
  [ "$WG_ENABLED" = "1" ] || { echo "[$NODE] 按要求跳过 WireGuard"; return; }
  local addr
  addr="$(awk -F'= *' '/^Address/{print $2}' "$WG_CONF" | tr -d ' ')"
  wg-quick strip "$WG_CONF" > /tmp/wg0.stripped
  ip link add wg0 type wireguard
  wg setconf wg0 /tmp/wg0.stripped
  ip addr add "$addr" dev wg0
  ip link set wg0 up
  wg show wg0 allowed-ips | awk '{print $2}' | while read -r net; do
    [ -n "$net" ] && ip route replace "$net" dev wg0 || true
  done
  echo "[$NODE] wg0 起来了 $addr，$(wg show wg0 peers | wc -l) 个 peer"
}

# For troubleshooting: LOGLEVEL=debug docker compose up -d hk-01
# The config is mounted read-only, so a copy is made, its loglevel edited, and that copy run,
# leaving the original untouched.
start_xray(){
  local conf="$XRAY_CONF"
  if [ -n "${LOGLEVEL:-}" ]; then
    conf=/tmp/xray.debug.json
    jq --arg lv "$LOGLEVEL" '.log.loglevel = $lv' "$XRAY_CONF" > "$conf"
    echo "[$NODE] loglevel=$LOGLEVEL（用的是 $conf，原配置没动）"
  fi
  echo "[$NODE] 启动 xray"
  exec xray run -config "$conf"
}

case "$START_ORDER" in
  wg-first)   bring_up_wg; start_xray ;;
  # Deliberately inverted: xray starts first with wg0 not yet present, to see what binding the
  # overlay does
  xray-first) ( sleep "${WG_DELAY:-15}"; bring_up_wg ) & start_xray ;;
  # For the full end-to-end scenario only: the container merely stays alive and wg0, xray, and
  # grants are all left to brocade-agent apply.
  agent-idle) echo "[$NODE] 等待 brocade-agent 收敛"; sleep infinity ;;
  *) echo "START_ORDER 只能是 wg-first、xray-first 或 agent-idle"; exit 1 ;;
esac
