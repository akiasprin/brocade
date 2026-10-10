# MTProxy protocol provenance

Telegram's own protocol and server are the behavioral authority for this package. The
implementation was reviewed against `TelegramMessenger/MTProxy` commit
`f36d8af769ffaeac36978d38c2c0f6d1104c2137` (2026-08-04) and the current official
transport documentation.

The relevant official sources are:

- `net/net-tcp-rpc-ext-server.c`: 64-byte obfuscated client header, transport selection,
  signed DC target and packet framing.
- `net/net-crypto-aes.c`: middle-proxy AES-CBC key and IV derivation.
- `net/net-tcp-rpc-client.c`: nonce and process handshake with Telegram middle proxies.
- `mtproto/mtproto-config.c`: signed DC routing and default-cluster fallback.
- `mtproto/mtproto-proxy.c`: `RPC_PROXY_REQ`, `RPC_PROXY_ANS`, quick acknowledgement and
  close handling.
- <https://core.telegram.org/mtproto/mtproto-transports>: abridged, intermediate, padded
  intermediate and transport-obfuscation wire formats.
- <https://core.telegram.org/proxy>: official service-data endpoints and deployment model.

No Telegram GPL source is copied into this directory. The Go implementation is designed as a
wire-compatible reimplementation. V2Ray's former MIT-licensed MTProto package was used only for
the Xray inbound registration and dynamic-user lifecycle shape; it is not the protocol
conformance authority.

## Deliberate differences

- Each client session currently owns one Telegram middle-proxy RPC connection. Telegram's C
  implementation pools and multiplexes four to eight middle connections per target. The simpler
  one-to-one shape preserves protocol behavior but costs an extra middle handshake per client.
- Brocade permits MTProxy only on a node with a direct, non-NAT public IPv4 address. Telegram's
  official server can be supplied an explicit NAT mapping, but Brocade deliberately does not
  discover or accept one: model validation rejects missing or NAT-marked public IPv4 nodes, and
  the middle-proxy key uses the real local socket address.
- Official `proxy-secret` and `proxy-multi.conf` data are fetched over HTTPS and cached in memory
  for 24 hours. A failed refresh keeps the last valid snapshot until the process restarts.
- The Telegram middle-proxy RPC connection uses a real system TCP socket from the ingress node.
  Its local and remote socket endpoints are inputs to the official AES key derivation, so this
  inbound does not use Xray's dispatcher, SNI/content sniffing, or domain/destination routing.
  It therefore exits directly from the ingress node rather than through an arbitrary Brocade
  chain hop.
- Brocade emits `dd` padded-intermediate links. TLS emulation (`ee` secrets) and promotion tags
  are not implemented yet and must not be advertised as supported.

## Verification

The default tests cover the official byte layouts, key derivation, framing, signed DC selection,
padding, quick acknowledgements, multi-user authentication and replay rejection:

```sh
go test ./proxy/mtproto ./infra/conf
go test -race ./proxy/mtproto
```

An opt-in compatibility test performs the official middle-proxy nonce and encrypted RPC
handshake with current service data:

```sh
XRAY_TEST_LIVE_MTPROTO=1 go test -run TestOfficialMiddleRPCHandshake -v ./proxy/mtproto
```

Run this gate on a direct-public-IPv4 production path. A NAT, transparent VPN or TUN that rewrites
the source address or port changes Telegram's middle-RPC key material and makes the handshake
fail; those paths are outside Brocade's supported MTProxy topology. Passing the default tests
alone is not sufficient to claim live Telegram compatibility.
