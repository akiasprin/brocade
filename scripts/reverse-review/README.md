# Xray reverse NAT review

Run from the repository root:

```sh
python3 scripts/reverse-review/run.py
```

Requires Go 1.26+, Python 3 and access to Docker. It builds the current working tree,
creates a disposable image with network tools, and runs independent privileged
containers with `--network none`. Network namespaces, SNAT and conntrack mutations
are confined to these containers. No host network namespace is entered or mounted.
Containers and the temporary image are removed afterwards. Allow about twelve minutes
plus build time. Raw probes, generated Xray configs, NAT tables, fault timestamps,
Xray logs and summaries are saved under `target/reverse-nat-review`.

Topology: portal (192.0.2.2) ← NAT (192.0.2.1 → 192.0.2.3) ← bridge (10.77.1.2).
Both Xray endpoints use VLESS reverse over raw TCP. A private echo service on the
bridge is explicitly allowed by its lab-only freedom outbound configuration.

- `flush_same`: delete the bridge's TCP conntrack entries, retain the SNAT address.
- `change_ip`: switch SNAT, then delete those entries; allow ordinary kernel RSTs.
- `change_ip_silent`: switch SNAT and delete entries, silently drop old tuples in
  both directions; allow new source ports. INPUT drops prevent router-generated RSTs
  after old NAT state disappears, while FORWARD drops cover reconstructed mappings.
- `downstream_silent`: retain SNAT address, delete entries and drop only the old
  return path; the bridge-to-portal direction is available.
- `change_ip_silent-keepalive5-2`: same silent IP change, override **bridge only**
  TCP keepalive idle/interval to 5/2 seconds. This is a diagnostic comparison.

Fresh TCP probes send a random 32-byte payload and verify the echo. Successful
probes start every 0.5 seconds; a failure may use the full 1-second timeout. Recovery
is measured from completion of fault injection, excluding in-flight probes.
`stable_recovery_s` starts the final successful run only if it contains at least
20 successes. These are observations at one workload, not a p99 or an SLA. Existing
application streams, TLS/REALITY setup and real WAN delays are not measured.

The `.go.txt` files are deliberately separate from ordinary passing tests. They
reproduce review findings without editing production code, via a Go overlay:

```sh
python3 - <<'PY'
import json, pathlib
r = pathlib.Path.cwd()
pairs = {
  'app/reverse/review_state_test.go': 'bridge_state_test.go.txt',
  'proxy/vless/inbound/review_empty_test.go': 'empty_dispatch_test.go.txt',
}
pathlib.Path('/tmp/reverse-review-overlay.json').write_text(json.dumps({'Replace': {
  str(r/'components/xray-core'/dest): str(r/'scripts/reverse-review'/source)
  for dest, source in pairs.items()
}}))
PY
cd components/xray-core
go test -race -overlay /tmp/reverse-review-overlay.json \
  ./app/reverse ./proxy/vless/inbound -run TestReview -count=1
```

Both tests are expected to fail on the reviewed tree: one reports a data race,
the other shows that an empty picker leaves a pipe caller without completion.
