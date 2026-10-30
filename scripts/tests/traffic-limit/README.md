# Machine traffic limit acceptance

This suite boots the real Agent and the selected current Xray in a disposable
Docker container and/or an actual KVM-accelerated Ubuntu cloud guest. The same
fixture runs in both environments. It uses an isolated veth peer namespace for
real TCP/UDP payload and NIC shaping, and a local HTTP work-delivery fixture;
Store accounting, authorization and migration semantics have separate PostgreSQL
integration tests.

The guest installs the selected binaries in its own root-owned private directory,
as the production installer does. It does not relax managed-binary ownership,
permission or restart-recovery checks to accommodate host-mounted input files.

Build the current binaries and supply a verified amd64 Ubuntu 24.04 cloud image:

```sh
cargo build -p brocade-agent --release --locked
# Build components/xray-core/main with the repository-pinned Go toolchain.
python3 scripts/tests/traffic-limit/run.py --runtime both \
  --fault-injection \
  --agent target/release/brocade-agent --xray /absolute/path/to/current/xray \
  --vm-image /absolute/path/to/verified/ubuntu-minimal.img \
  --output /absolute/path/to/new-results
```

`--runtime docker` does not require a VM image. `--docker-image` selects an image
with Python 3 and iproute2 (the default is built by
`../forwarding-continuity/Dockerfile`). `--qemu-image` selects tooling with
`qemu-system-x86_64`, `qemu-img`, and `cloud-localds`. KVM requires `/dev/kvm`; the
runner uses `-accel kvm` and does not silently fall back to emulation. The supplied
image must contain Python, iproute2 and kmod; missing tools produce an explicit
failure. `--provision-packages` opts into cloud-init package installation, which
requires Internet access. The default uses the verified image without package
updates; the Docker node runs with `--network none`.

The Docker node requires NET_ADMIN/SYS_ADMIN and an unconfined seccomp/AppArmor
profile to create its private peer namespace. Neither runtime uses host network
or PID namespaces. The QEMU tooling container gets only `/dev/kvm` plus the
laboratory files; it does not get host network-management capabilities. All
network changes happen inside the disposable node/guest. Do not run `guest.py`
on the host. Cleanup targets only exact newly created container names.

Acceptance includes a 90% cumulative threshold, 100 Mbps RX/TX, connection-shared
caps, stricter absolute thresholds while Console delivery is unavailable,
disable/cleanup, application scope, TCP and atomic UDP at 1 Kbps, missing `tc`
with successful configuration handoff and application protection, active-budget
outage, recoverable budget initialization errors, Agent restart, container
restart or actual KVM reboot, and offline restoration of durable meters/caps.

`--fault-injection` adds invalid/truncated/stale control-plane deliveries, damaged
policy/network/application checkpoints, oversized files, FIFOs, leaf symlinks,
live control-file loss, restart recovery,
owned HTB default-class drift, and preservation of foreign root/ingress/egress
rules. It checks that a broken system layer still permits application updates and
disable. The flag is recorded in the manifest and independently required by the
verifier; it changes only the disposable environment, never host or fleet rules.

The fixture's bounded checkpoint readers have a host-safe regression test:
`python3 -m unittest discover -s scripts/tests/traffic-limit -p 'test_*.py'`.

NIC adoption and cleanup also have Agent-only kernel regressions. Set
`BROCADE_TRAFFIC_LIMIT_NET_IMAGE` to an image containing full iproute2 and run:

```sh
BROCADE_TRAFFIC_LIMIT_NET_IMAGE=brocade-forwarding-continuity:local \
  cargo test -p brocade-agent --locked traffic_limit_ -- --ignored \
  --skip traffic_limit_netlink_guest --skip traffic_limit_real_xray_generations \
  --test-threads=1 --nocapture
```

Both selected tests must pass; an empty match is not acceptance. The additional
compatibility fixture needs host `/dev/net/tun`, mounted only into its disposable
container. It creates a persistent multiqueue TAP there to obtain a real
kernel-default `mq 0:` with `:1` parents, rather than synthesizing that JSON. Cases
cover repeated adoption/disable, legacy ingress, shared ingress and egress blocks,
foreign filters on both owned HTB roots, and IPv4-only redirect drift. Existing
cases cover ordinary schedulers, HTB default drift, exact 32/64-bit rates and
external shaping preservation. The tests share only a container helper; the
netlink guest entry retains its existing exact test name.

When the fixture image has older glibc than the host, build the test executable
for `x86_64-unknown-linux-musl` using the repository's configured cross toolchain
and add that `--target` to the command. A guest that cannot start the executable
is a failed fixture, not a successful compatibility check. Compare iproute2
versions using separate images; these container runs share the host Linux kernel
and do not certify older kernels or ARM64.

Results retain the binary and fixture SHA-256, guest OS/kernel, target/measured
throughput, real configuration receipts, logs, and restart evidence. KVM must
produce a different kernel boot ID; Docker records different container start
times. Speed gates allow measurement overhead and TCP headers; this local
veth acceptance does not certify WAN loss, all distributions or ARM64.

After completion, independently check the entire case matrix and retained inputs:

```sh
python3 scripts/tests/traffic-limit/verify.py /absolute/path/to/results
```
