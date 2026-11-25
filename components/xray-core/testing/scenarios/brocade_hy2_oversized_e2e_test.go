package scenarios

import (
	"bytes"
	"fmt"
	stdnet "net"
	"testing"
)

func TestBrocadeFrozenHY2SalamanderOversizedDatagrams(t *testing.T) {
	frozenBinary(t)
	echo := frozenEcho(t)
	frozenPacketEcho(t, echo)
	entry := frozenPort(t)
	mask := frozenObject{"udp": []any{frozenObject{"type": "salamander", "settings": frozenObject{"password": "oversized-datagram-fixture"}}}}
	server := frozenIngress(t, "hysteria", entry)
	server["streamSettings"].(frozenObject)["finalmask"] = mask
	frozenStart(t, frozenObject{"inbounds": []any{server}, "outbounds": frozenFreedom()})
	injector := frozenUDPDial(t, entry)
	inject := func() {
		t.Helper()
		// Salamander does not authenticate a datagram before decoding it. An
		// unsolicited packet larger than QUIC's receive buffer must be a drop.
		if _, err := injector.Write(bytes.Repeat([]byte{0x5a}, 2000)); err != nil {
			t.Fatal(err)
		}
	}
	newClient := func() int {
		t.Helper()
		local := frozenPort(t)
		wire := frozenWire(t, "hysteria", false)
		wire["hysteriaSettings"].(frozenObject)["auth"] = frozenAlice
		wire["finalmask"] = mask
		in := frozenDoko(local, echo)
		in["settings"].(frozenObject)["network"] = "tcp,udp"
		out := frozenObject{"protocol": "hysteria", "settings": frozenObject{"version": 2, "address": "127.0.0.1", "port": entry}, "streamSettings": wire}
		frozenStart(t, frozenObject{"inbounds": []any{in}, "outbounds": []any{out}})
		return local
	}

	inject()
	first := newClient()
	tcp := frozenDirect(t, first, "127.0.0.1")
	udp := frozenUDPDial(t, first)
	for round := range 2 {
		if round > 0 {
			inject()
		}
		for label, conn := range map[string]stdnet.Conn{"tcp": tcp, "udp": udp} {
			payload := []byte(fmt.Sprintf("existing-%s-after-oversized-datagram-%d", label, round))
			if err := frozenRoundtrip(conn, payload); err != nil {
				t.Fatalf("%s forwarding failed after oversized packet: %v", label, err)
			}
		}
	}
	second := newClient()
	for label, conn := range map[string]stdnet.Conn{"tcp": frozenDirect(t, second, "127.0.0.1"), "udp": frozenUDPDial(t, second)} {
		if err := frozenRoundtrip(conn, []byte("new-connection-after-oversized-datagram")); err != nil {
			t.Fatalf("new %s connection failed after oversized packet: %v", label, err)
		}
	}
	t.Log("same Xray listener kept existing TCP/UDP traffic and accepted a fresh QUIC connection after oversized datagrams")
}
