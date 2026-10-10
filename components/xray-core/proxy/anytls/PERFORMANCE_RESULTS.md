# AnyTLS performance results

This file records reproducible measurements for data-path changes. Results are
comparisons on one host, not capacity promises for production nodes.

## 2026-10-09: TX-only validation across P-cores and E-cores

The TX-only implementation was retested after removing kTLS RX. The benchmark
first had to be corrected: Xray access/error logs were disabled during the
measurement window, load-generator CPU was added to the process counters, and
`GOMAXPROCS` was raised from 4 to 8. The server is still hard-pinned to the
listed one or two logical CPUs, while the client and the inner HTTPS workload
can use all eight logical CPUs on four separate P-cores. Without that change,
a four-core server run measured an artificially constrained client/load side.

### Environment and acceptance gates

- Candidate: `7fd6c7d6e377` plus the TX-only working-tree changes; compiled
  scenario binary SHA-256
  `8c44b27f8baa3883a6bcc3500d97ce610609b960bedb6387df195de4dc0469c3`.
- Linux `7.0.0-34-generic`, Go `1.26.0`, Intel Core i7-13700, clock tick 100 Hz.
- P1 used CPU 8; P2 used CPUs 8 and 10. These are distinct physical P-cores
  and their SMT siblings were excluded from the server affinity. E1 used CPU
  20; E2 used CPUs 20 and 21, which are distinct non-SMT E-cores.
- Client and load generator were pinned to CPUs 0-7. The sustained ChatGPT GPU
  worker was temporarily moved off the server cores and restored afterwards.
  A root-owned QEMU workload using about 20% of one CPU could not be moved, so
  this is an affinity-controlled workstation result, not a bare-metal lab run.
- Each run used 128 independent connections and 64 MiB per connection (8 GiB
  total), with one full-size warm-up and five measured runs for every
  topology/direction/mode. Modes were interleaved, and topology order was
  reversed on alternating rounds. Tables show the median and observed range;
  changes are medians of the five same-round pairs.
- This scenario tunnels fragmented TLS 1.3 HTTPS through AnyTLS, so the client
  and load side also perform real TLS and hash validation. Multi-core results
  therefore include the measured end-to-end loopback ceiling.
- All interfaces reported fixed-off TLS hardware TX/RX offload. Every one of
  the 40 `required` runs added exactly 129 `TlsTxSw` sessions (128 transfers
  plus the startup probe). All 40 `off` runs added zero. `TlsRxSw`,
  `TlsTxDevice`, and `TlsDecryptError` remained unchanged in all 80 measured
  runs. The result is software kTLS TX plus Go TLS RX, as intended.
- Package temperature was 76-84 C (79 C median before and after runs). The
  selected P-cores accumulated 200 throttle events across 40 P-core runs; the
  selected E-cores accumulated none. P-core values are consequently sustained
  performance under this host's present cooling, not an unthrottled peak.

One P1 download run can be reproduced with the following command. The full
matrix changes the server CPU list to `20`, `8,10`, or `20,21`, alternates
`download`/`upload` and `off`/`required`, and uses the interleaving described
above.

```sh
go test -c -o /tmp/brocade-anytls-perf.test ./testing/scenarios
env GOMAXPROCS=8 XRAY_LOCAL_PERF=1 \
  XRAY_LOCAL_PERF_CONCURRENCY=128 XRAY_LOCAL_PERF_BODY_BYTES=67108864 \
  XRAY_LOCAL_PERF_SERVER_CPUS=8 XRAY_LOCAL_PERF_CLIENT_CPUS=0-7 \
  XRAY_LOCAL_PERF_LOAD_CPUS=0-7 XRAY_LOCAL_PERF_DIRECTION=download \
  'xray.anytls.ktls=required' 'xray.anytls.writev=auto' \
  'xray.anytls.splice=off' 'xray.buf.splice=auto' \
  /tmp/brocade-anytls-perf.test \
  -test.run '^TestLocalAnyTLSHighConcurrency$' -test.v -test.timeout=2m
```

### Single-core result

| Direction / server | Full Go TLS GiB/s | kTLS TX GiB/s | Paired throughput change | Full Go TLS CPU s/GiB | kTLS TX CPU s/GiB | Paired CPU change |
|---|---:|---:|---:|---:|---:|---:|
| Download, P1 | 1.45 [1.43, 1.54] | 1.84 [1.78, 1.86] | +27.3% [+17.9%, +28.7%] | 0.684 | 0.539 | -21.5% |
| Download, E1 | 0.80 [0.80, 0.81] | 0.87 [0.87, 0.88] | +8.8% [+7.4%, +10.0%] | 1.241 | 1.144 | -7.8% |
| Upload, P1 | 1.70 [1.67, 1.72] | 1.69 [1.67, 1.71] | neutral (+0.6%) | 0.583 | 0.586 | neutral (-0.6%) |
| Upload, E1 | 1.07 [1.07, 1.09] | 1.07 [1.07, 1.08] | neutral (0.0%) | 0.924 | 0.926 | neutral (+0.3%) |

Both single-core server modes were effectively saturated (98.8-99.6% median
server utilization). On download, kTLS TX reduced server read syscalls by
51.8-52.6% and write syscalls by 88.6-88.8%. Upload remains on Go TLS RX and
all paired throughput and CPU changes stayed inside a +/-2.6% noise band.

### Multi-core result

| Direction / server | Full Go TLS GiB/s | kTLS TX GiB/s | Paired throughput change | Full Go TLS CPU s/GiB | kTLS TX CPU s/GiB | Paired CPU change |
|---|---:|---:|---:|---:|---:|---:|
| Download, P2 | 2.37 [2.32, 2.39] | 2.38 [2.34, 2.39] | platform ceiling (0.0%) | 0.708 | 0.480 | -32.5% |
| Download, E2 | 1.47 [1.45, 1.48] | 1.65 [1.65, 1.66] | +12.9% [+11.5%, +13.8%] | 1.341 | 1.199 | -10.7% |
| Upload, P2 | 1.68 [1.68, 1.72] | 1.68 [1.64, 1.72] | neutral (-0.6%) | 0.760 | 0.752 | neutral (+1.7%) |
| Upload, E2 | 1.73 [1.69, 1.74] | 1.73 [1.71, 1.76] | neutral (+1.2%) | 1.080 | 1.083 | neutral (-0.1%) |

E2 download saturated both server cores and retained a 12.9% throughput gain.
P2 download reached the workstation's client/load ceiling at about 2.38 GiB/s:
server utilization fell from 83.1% to 56.7% with kTLS while throughput stayed
flat. That row is evidence of 32.5% lower server CPU per GiB and additional
headroom, not evidence that kTLS has no multi-core benefit. A P4 pilot was
rejected as a server-capacity measurement because only about 1.9 of its four
server cores were busy; it measured the same-machine peer ceiling instead.

The acceptance result is therefore direction-specific: keep software kTLS for
TX, where every saturated P/E-core download test improved and server CPU fell;
keep RX on Go TLS, where upload stayed neutral and the earlier kTLS RX tests
regressed materially.

## 2026-10-09: keep RX on Go TLS

The original promotion installed both `TLS_TX` and `TLS_RX`, although Linux
treats them as independent directions. AnyTLS has a dedicated 128 KiB kTLS
writev path for TX, but RX fed one 8 KiB pooled buffer at a time to `recvmsg`.
After Go's TLS writer reached its steady 16 KiB record size, software kTLS RX
had to decrypt into kernel buffers and serve each record through multiple
reads.

The corrected capacity test pinned the server to an otherwise idle, non-SMT
E-core and gave the client and load generator separate cores. Each run used
128 connections and transferred 8 GiB; modes were interleaved, and the table
reports the median of five runs. Both server modes consumed effectively the
entire assigned core.

| Direction | kTLS off | Bidirectional kTLS | Change |
|---|---:|---:|---:|
| Download throughput | 0.779 GiB/s | 0.851 GiB/s | +9.3% |
| Download server CPU | 1,022 ticks | 935 ticks | -8.5% |
| Upload throughput | 1.060 GiB/s | 0.761 GiB/s | -28.2% |
| Upload server CPU | 750 ticks | 1,028 ticks | +37.1% |

A 256 MiB upload syscall trace attributed 49,271 additional `recvmsg` calls
to kTLS RX. A seven-second server CPU profile moved from 53.2% syscall and
23.9% Go AES-GCM time to 82.2% syscall time. There were no TLS decrypt errors,
padding retries, or hardware-offloaded sessions.

The production path now installs only `TLS_TX`. Server reads remain on Go TLS,
but the transport implements `ReadMultiBuffer` with a 16 KiB pooled buffer so
one call can carry a complete TLS plaintext record into AnyTLS. The existing
kTLS writev/splice and session-ticket TX sequence handling remain unchanged.
TX-only promotion also removes the TLS-record-boundary read wrapper that was
needed when handing RX to the kernel. Leaving that wrapper in place forced
every record header and body through separate reads: a first TX-only candidate
still performed about 1.45 million server read syscalls and only 0.95 GiB/s on
upload.

Two upload candidates were measured separately under the same isolation:

| Experiment | Throughput | Server CPU | Throughput change | CPU change |
|---|---:|---:|---:|---:|
| Go TLS read buffer, 8 KiB -> 16 KiB | 1.061 -> 1.090 GiB/s | 750 -> 732 ticks | +2.7% | -2.4% |
| Time-matched Go TLS -> vectored kTLS RX + authenticated no-pad | 1.041 -> 0.815 GiB/s | 763 -> 952 ticks | -21.8% | +24.8% |

The kTLS RX run had no decrypt errors or no-pad violations. With no TLS RX
device offload on this host, moving AES-GCM receive processing into the kernel
was intrinsically slower even after eliminating split-record `recvmsg` calls.
It was rejected; the 16 KiB Go TLS receive path is the landed upload
optimization.

The final path was retested under the same CPU isolation with three interleaved
pairs. Values below are medians; differences below 1% are treated as neutral.

| Direction / server metric | Full Go TLS | kTLS TX + Go TLS RX | Change |
|---|---:|---:|---:|
| Download throughput | 0.779 GiB/s | 0.853 GiB/s | +9.6% |
| Download CPU | 1,025 ticks | 935 ticks | -8.8% |
| Upload throughput | 1.090 GiB/s | 1.082 GiB/s | neutral (-0.7%) |
| Upload CPU | 732 ticks | 734 ticks | neutral (+0.3%) |
| Upload read syscalls | 280,685 | 282,459 | neutral (+0.6%) |

The earlier rejected RX experiment below used an end-to-end test in which the
server was not the proven bottleneck; it is retained as historical context
rather than an acceptance result for RX capacity.

## 2026-10-05: pooled kTLS writev

### Environment and method

- Baseline: `6f7de98eddc9`.
- Candidate: fixed 128 KiB batches with pooled frame headers, Go vectors,
  native iovecs, and the raw-connection callback.
- Linux `7.0.0-34-generic`, Go `1.26.0`, Intel Core i7-13700.
- Real-process runs used 32 independent AnyTLS/TLS connections, 128 MiB per
  connection (4 GiB total), `GOMAXPROCS=1`, server/client/load pinned to
  separate physical cores 1/3/5, kTLS `required`, writev `auto`, splice `off`.
- Baseline and candidate runs were interleaved in the same time window. Tables
  report the median of three complete real-process runs.

### Pooled kTLS writev microbenchmark

Command:

```sh
taskset -c 1 env GOMAXPROCS=1 \
  go test ./proxy/anytls -run '^$' \
  -bench '^BenchmarkWritePSHBatchVectored$' \
  -benchmem -benchtime=3s -count=5
```

| Path | ns/op | B/op | allocs/op | Change from baseline |
|---|---:|---:|---:|---:|
| baseline | 18,820 | 410 | 7 | - |
| candidate | 18,049 | 0 | 0 | time -4.1%; allocations -100% |

The native iovec array is reused because `unix.Writev` allocates once the call
exceeds its small inline iovec capacity. The write remains serialized by the
existing AnyTLS session write lock, and partial writes retain the original
retry semantics.

### Real HTTPS fragmented download

This direction stresses the server's framed kTLS write path.

| Server metric | Baseline | Candidate | Change |
|---|---:|---:|---:|
| CPU ticks | 158 | 155 | -1.9% |
| elapsed | 4.4655 s | 4.4583 s | -0.2% |
| throughput | 0.90 GiB/s | 0.90 GiB/s | neutral |
| read syscalls | 45,652 | 46,842 | +2.6% |
| write syscalls | 46,547 | 47,684 | +2.4% |
| RSS | 44,367,872 B | 40,751,104 B | -8.2% |

Run-to-run syscall values vary with TCP fragmentation, and the pooled writev
does not claim to reduce syscall count. The stable acceptance signal is zero
steady-state benchmark allocations, modest CPU/RSS improvement, and no
regression in real-process elapsed time or throughput. This is an allocation
optimization, not a material capacity increase.

### Rejected experiments

- `TLS_RX_EXPECT_NO_PAD` plus vectored kTLS receive reduced server CPU ticks by
  2.6% in the upload profile, but elapsed time increased 4.1%, throughput fell
  3.9%, and peak RSS increased 3.5%. It was removed from the final path.
- A 256 KiB single-writer batch did not reduce syscalls because fragmented TCP
  reads commonly completed before the buffer filled. Read and write syscalls
  increased about 6%, and each active writer retained a larger buffer. The
  final path keeps the existing 128 KiB fairness limit.

### TLS 1.3 session resumption correctness

The former kTLS promotion path forced `SessionTicketsDisabled=true`, even when
the TLS configuration explicitly enabled resumption. The independent
correctness fix preserves that configuration and initializes the kTLS TX
record sequence after the NewSessionTicket records emitted during the
handshake. Loopback tests verify a resumed second connection with both the
default ticket and a custom ticket large enough to span two TLS records.

Session resumption remains disabled by the existing default configuration and
is not included in the bulk-throughput benefit above.

### Reproduction

```sh
env GOMAXPROCS=1 XRAY_LOCAL_PERF=1 \
  XRAY_LOCAL_PERF_CONCURRENCY=32 XRAY_LOCAL_PERF_BODY_BYTES=134217728 \
  XRAY_LOCAL_PERF_SERVER_CPUS=1 XRAY_LOCAL_PERF_CLIENT_CPUS=3 \
  XRAY_LOCAL_PERF_LOAD_CPUS=5 XRAY_LOCAL_PERF_DIRECTION=download \
  'xray.anytls.ktls=required' 'xray.anytls.writev=auto' \
  'xray.anytls.splice=off' \
  go test ./testing/scenarios -run '^TestLocalAnyTLSHighConcurrency$' \
  -count=3 -v -timeout=4m
```
