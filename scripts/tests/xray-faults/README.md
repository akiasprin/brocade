# Production Agent / Xray Docker fault injection

This suite runs the actual Agent CLI, its resident guard, HTTP work delivery,
spools and real Xray processes in disposable Docker PID/network namespaces.
It uses no production endpoints, host networking, privileged mode, synthetic
Xray responses or test-only Agent failpoints. The HTTP fixture supplies frozen
work and acknowledges receipts; it does not implement PostgreSQL release policy.

Build the tooling image and current Agent, and select an explicit Xray built
from the current fork:

```sh
docker build -t brocade-xray-faults:local -f scripts/tests/xray-faults/Dockerfile scripts/tests/xray-faults
cargo build -p brocade-agent --release --locked
python3 scripts/tests/xray-faults/run.py \
  --agent target/release/brocade-agent --xray /absolute/path/to/current/xray \
  --output /absolute/path/to/new-results --repeat 2 --jobs 3
```

Use the release build for resource-constrained timing measurements; debug builds have substantially more hashing overhead. Run `python3 scripts/tests/xray-faults/verify.py RESULTS` after completion to independently check the full case set, frozen identities, real child statuses and raw payload evidence. Results directories must be new. The runner freezes the Agent and fixture source,
records their SHA-256 plus Xray/image identity, and retains per-case child status,
Agent/Xray logs, state files, requests, payload verification, grants API readback,
usage samples and receipt evidence. Binary cases append harmless bytes to the
selected ELF to exercise a real different-digest replacement, not a same-hash
no-op. They certify the executor, not cross-version data-plane compatibility.

`--cases` accepts a comma-separated subset from `CASES` in `run.py`.
`--cpus` defaults to `2`; `--jobs` is bounded to 1–8. Each container has its own
network with loopback only, 768 MiB memory and 512 PID limit. A permitted private
test address is assigned to that isolated loopback using `NET_ADMIN`; managed
forwarding retains the production loopback/metadata deny policy. `--init` reaps
orphaned children as a host init would. ENOSPC uses a
separate 64 MiB executable tmpfs; the host filesystem is never filled. Cleanup
uses exact randomly owned container names, with retained results on failure.

The fixture provisions an explicit empty source-access checkpoint and starts the
real resident admission service before checking forwarding. Its local Console
returns explicit unlimited test budgets. Repeated-handoff retention waits for
the production 30-second pruning cadence and bounded four-directory batches;
that case has a 180-second process limit, other cases retain 100 seconds.

The matrix covers:

- configuration and binary handoff, natural/forced drain, new listeners;
- eight consecutive configuration handoffs in one node state directory, with bounded
  retired executable/log retention and live-generation protection;
- uninterrupted existing plain TCP and REALITY/Vision TLS payload streams;
- repeated short TCP, UDP and authenticated VLESS requests;
- 64, 1,024 and 4,096 grants, changed UUIDs and explicit restart fallback;
- Agent exit while switching/draining, old/candidate/both process loss;
- stopped processes, pre-admission candidate freeze, lost control socket and
  a filled Unix accept backlog;
- legacy launch without a handoff manifest, bad config, runtime bind collision,
  bad binary digest/version, ENOSPC, Console/receipt outage and lost ACK replay.

The config force-drain case explicitly advances the persisted deadline to avoid
waiting 120 seconds per iteration; binary cases use the real 10-second offer
minimum. Phase-triggered injections must actually observe their requested phase.
A missed phase is a failure, not a skipped or successful injection.

Every case requires a valid initial business baseline and a bounded terminal
state with one managed Xray. Invalid candidates must retain the original PID.
Healthy handoffs require existing payload streams to advance across admission.
Final verification includes dynamic forwarding, UDP, exact grant UUID readback,
new config listeners and receipt status. The measured largest gap between
successful request starts, including a failed tail, must stay below 12 seconds.
This is a regression gate for this fixture, not a production outage SLA; requests
use a 500 ms deadline (Vision uses 2 seconds), and deployment receipt completion
can lag business recovery by a 15-second convergence round.

Existing native tests complement the external suite with explicit empty/missing
frozen grants, IPv6 API/metrics, accounting promotion and interrupted recovery
checkpoints. Run serially in a disposable container with `BROCADE_TEST_XRAY`:

```sh
cargo test -p brocade-agent --locked --no-run
# Mount the exact emitted test executable and Xray read-only into a new container:
# BROCADE_TEST_XRAY=/lab/xray /lab/agent-tests xrayupdate::tests::native_ --ignored --test-threads=1
```

Store/Console billing and idempotency need their separate PostgreSQL integration
tests. This suite does not certify WAN loss/latency, ARM64, systemd/OpenRC service
supervision, power-loss filesystem durability, MPTCP, Hy2/XHTTP/AnyTLS or a real
old-to-new Xray version transition. Disk/checkpoint corruption remains fail-closed
and requires repair; availability does not authorize accepting unverifiable grants.
