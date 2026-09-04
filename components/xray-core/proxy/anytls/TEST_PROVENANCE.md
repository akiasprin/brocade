# AnyTLS Test Provenance

This file records where the AnyTLS test scenarios came from and how they were
independently expressed for the Brocade Xray fork. It is an engineering
provenance record, not a legal opinion.

## Fixed references

- Protocol and URI semantics: `anytls/anytls-go` at
  `fd6167acd6d73b9fa3e607659951847fbc9e6c50`, especially
  `docs/protocol.md`, `docs/uri_scheme.md`, and `docs/client-name.md`.
- Host integration shape: `SagerNet/sing-box` at
  `0b8995879f29a9b98ee027bc17b75e101445b238`.
- Observable large-write behavior: `anytls/sing-anytls` at
  `479cb5bd490a2f4b1b6e8cd82b821afb392a94c8`, especially
  `session/session_test.go`.
- Observable lifecycle and concurrency scenarios: `SagerNet/sing-anytls` at
  `43bb1cbea74e889b35d73b89827fa355d9d98c21`, especially
  `test/scenario_test.go`, `test/wire_test.go`, and `test/stress_test.go`.

The two `sing-anytls` repositories are GPLv3 references. No source file,
helper, fixture, test body, function structure, or assertion text was copied
or mechanically translated. Tests below use Xray-native `buf`, `pipe`,
`transport.Link`, dispatcher, and loopback TCP fixtures.

## Scenario map

| ID | Behavior protected | Source | Independent fixture and extensions | Tests |
|---|---|---|---|---|
| AT-001 | Frame, strict command shapes, settings, padding, text-length, truncation, and short-write boundaries | Protocol documents | Hand-built wire bytes and Xray buffers; includes malformed direction/ID/body combinations, packet-0 limits, malformed/truncated input, and 65,535-byte boundaries | `frame_test.go`, `session_wire_test.go`, `settings_test.go`, `padding_test.go`, `utils_test.go` |
| AT-002 | Authentication, static/dynamic user uniqueness, concurrent index updates, session revocation, and masquerade separation | Protocol documents plus Xray HandlerService behavior | In-memory Xray accounts, concurrent add attempts, static server construction, test connections, and Xray server state; no third-party fixture | `inbound_auth_test.go`, `user_test.go`, `masquerade_test.go` |
| AT-003 | Writes larger than one frame remain ordered and contiguous | `anytls/sing-anytls/session/session_test.go` | Deterministic patterned payloads through Xray frame writers and `transport.Link`; also checks ownership and write-lock release | `frame_test.go`, `session_client_test.go`, `session_integration_test.go` |
| AT-004 | SYN ordering and stream-ID retirement, one-shot ServerSettings, multiplexing, peer FIN, blocked delivery, duplicate SYNACK, idle-session replacement, and close races | Protocol documents and `SagerNet/sing-anytls/test/wire_test.go` | Fresh Xray session harness with explicit frame parsing, concurrent client opens, a gated idle-session close between pool selection and stream allocation, dispatcher links, and lifecycle assertions | `session_wire_test.go`, `session_client_test.go`, `session_lifecycle_test.go`, `session_integration_test.go` |
| AT-005 | Concurrent stream progress and concurrent session close | Scenario intent from `SagerNet/sing-anytls/test/stress_test.go` | Real loopback TCP pair, 32 workers x 8 streams, mixed frame sizes, omitted downlink consumers, 64-stream concurrent close | `stress_test.go` |
| AT-006 | Goroutine, file-descriptor, stream-map, and active-stream convergence | Brocade extension | 64 complete real-TCP session lifecycles after warm-up; Linux `/proc/self/fd`, runtime goroutine baseline, and timeout diagnostics | `leak_test.go` |
| AT-007 | UoT v2 records, zero-length UDP payloads, destination metadata, and TCP multiplexing | Protocol documents | Xray packet buffers and dispatcher echo; zero-length datagrams are a Brocade/Xray boundary extension | `session_integration_test.go`, `fuzz_test.go`, `common/buf/multi_buffer_test.go`, `transport/pipe/pipe_test.go` |
| AT-008 | Parsers and session state remain bounded for arbitrary input up to 1 MiB | Brocade extension | Go native fuzzing with independently selected seeds; generated regression corpus is retained under `testdata/fuzz` | `fuzz_test.go`, `testdata/fuzz/FuzzAnyTLSSessionFrames` |

## Gate mapping

- Deterministic repetition: `go test ./common/buf ./transport/pipe ./proxy/anytls -count=10 -timeout=5m`
- Race and lifecycle repetition: `go test -race ./common/buf ./transport/pipe ./proxy/anytls -count=3 -timeout=10m`
- Timed fuzzing: all four `FuzzAnyTLS*` targets run for 10 seconds each in CI.
- Release candidates still require the full bilateral interoperability matrix;
  these package-level tests do not replace it.

Engineering provenance is complete for the listed tests. Final license review
remains the responsibility of the project owner before release.
