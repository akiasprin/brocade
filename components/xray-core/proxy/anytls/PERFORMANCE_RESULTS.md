# AnyTLS performance results

This file records reproducible measurements for data-path changes. Results are
comparisons on one host, not capacity promises for production nodes.

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
