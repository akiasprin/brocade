# Forwarding continuity and rule status

Runs the production Console, PostgreSQL, Agent and an explicitly selected Xray in
disposable Docker containers. The network is internal, no host ports are published,
and the only extra capability is NET_ADMIN to assign a local target address inside
the test namespace. No production endpoint or credential is used.

```sh
docker build -t brocade-forwarding-continuity:local scripts/tests/forwarding-continuity
cargo build -p brocade-console -p brocade-agent --locked
python3 scripts/tests/forwarding-continuity/run.py \
  --xray /absolute/path/to/xray \
  --output /absolute/path/to/new-results
```

`--agent`, `--console` and `--image` override the default local builds and tooling
image. Results directories must be new. Run once with the current embedded Xray
and once with an older Xray when checking rolling-upgrade compatibility.

The test holds six original TCP sockets without reconnecting, and continuously
checks short TCP connections, persistent UDP sessions and fresh UDP sockets.
It verifies TCP/UDP/dual additions, another user's addition, an actual port bind
collision, automatic recovery after releasing the port, a metadata-only rename
and a target edit. Unchanged rules must remain active through every mutation;
only the colliding rule may report an error. Xray's PID and start ticks must stay
unchanged. Changing the control rule's target must close that rule's old TCP socket
while the original probes keep advancing.

`result.json` retains per-scenario counters, statuses, exact payload checks and
connection errors. API snapshots, final PostgreSQL state, Agent/Xray logs and a
frozen runner/binary manifest are also retained. Containers and their private
network are removed by exact randomly owned names. The fixture does not certify
WAN loss, process supervision, config/binary handoff or a zero-downtime target edit.
