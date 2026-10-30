package scenarios

import (
	"bytes"
	"fmt"
	stdnet "net"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"
)

// Two destination ports model the node's DNAT range. One backend socket keeps
// the QUIC endpoint stable while real client datagrams arrive at distinct ports.
func frozenHopGateway(t *testing.T, destination int) ([]int, []*atomic.Int64, *atomic.Int64) {
	t.Helper()
	backend, err := stdnet.DialUDP("udp", nil, &stdnet.UDPAddr{IP: stdnet.IPv4(127, 0, 0, 1), Port: destination})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = backend.Close() })
	var mu sync.Mutex
	peers := map[string]bool{}
	unique := new(atomic.Int64)
	var last *stdnet.UDPAddr
	var selected *stdnet.UDPConn
	go func() {
		data := make([]byte, 65536)
		for {
			n, err := backend.Read(data)
			if err != nil {
				return
			}
			mu.Lock()
			peer, socket := last, selected
			mu.Unlock()
			if peer != nil {
				_, _ = socket.WriteToUDP(data[:n], peer)
			}
		}
	}()
	var ports []int
	var counters []*atomic.Int64
	for range 2 {
		listener, err := stdnet.ListenUDP("udp", &stdnet.UDPAddr{IP: stdnet.IPv4(127, 0, 0, 1)})
		if err != nil {
			t.Fatal(err)
		}
		t.Cleanup(func() { _ = listener.Close() })
		ports = append(ports, listener.LocalAddr().(*stdnet.UDPAddr).Port)
		count := new(atomic.Int64)
		counters = append(counters, count)
		go func() {
			data := make([]byte, 65536)
			for {
				n, peer, err := listener.ReadFromUDP(data)
				if err != nil {
					return
				}
				count.Add(1)
				mu.Lock()
				last, selected = peer, listener
				if !peers[peer.String()] {
					peers[peer.String()] = true
					unique.Add(1)
				}
				mu.Unlock()
				_, _ = backend.Write(data[:n])
			}
		}()
	}
	return ports, counters, unique
}
func TestBrocadeFrozenLegacyUDPHopAndGSO(t *testing.T) {
	frozenBinary(t)
	echo := frozenEcho(t)
	server := frozenPort(t)
	frozenStart(t, frozenObject{"inbounds": []any{frozenIngress(t, "hysteria", server)}, "outbounds": frozenFreedom()})
	ports, counts, unique := frozenHopGateway(t, server)
	local := frozenPort(t)
	wire := frozenWire(t, "hysteria", false)
	wire["hysteriaSettings"].(frozenObject)["auth"] = frozenAlice
	wire["finalmask"] = frozenObject{"quicParams": frozenObject{"disableChromeParrot": true, "udpHop": frozenObject{"ports": fmt.Sprintf("%d,%d", ports[0], ports[1]), "interval": "5-5"}}}
	outbound := frozenObject{"protocol": "hysteria", "settings": frozenObject{"version": 2, "address": "127.0.0.1", "port": ports[0]}, "streamSettings": wire}
	trace := filepath.Join(t.TempDir(), "sendmsg.log")
	frozenStart(t, frozenObject{"inbounds": []any{frozenDoko(local, echo)}, "outbounds": []any{outbound}}, "BROCADE_E2E_SYSCALL_FILE="+trace)
	c := frozenDirect(t, local, "127.0.0.1")
	payload := bytes.Repeat([]byte{0x5a}, 512*1024)
	_ = c.SetDeadline(time.Now().Add(10 * time.Second))
	if err := frozenRoundtrip(c, payload); err != nil {
		t.Fatal(err)
	}
	// Wait for a real scheduled hop; keep the original application socket.
	deadline := time.Now().Add(13 * time.Second)
	for time.Now().Before(deadline) && unique.Load() < 2 {
		time.Sleep(300 * time.Millisecond)
		if err := frozenRoundtrip(c, []byte("same-stream-after-port-hop")); err != nil {
			t.Fatal(err)
		}
	}
	if unique.Load() < 2 {
		t.Fatalf("no real port hop: %d/%d", counts[0].Load(), counts[1].Load())
	}
	if err := frozenRoundtrip(c, payload); err != nil {
		t.Fatal(err)
	}
	calls, err := os.ReadFile(trace)
	if err != nil {
		t.Fatal(err)
	}
	if root := os.Getenv("BROCADE_E2E_OUTPUT"); root != "" {
		if err := os.WriteFile(filepath.Join(root, "udphop-sendmsg.log"), calls, 0o600); err != nil {
			t.Fatal(err)
		}
	}
	if !strings.Contains(string(calls), "UDP_SEGMENT") && !strings.Contains(string(calls), "cmsg_level=SOL_UDP, cmsg_type=0x67") {
		t.Fatalf("real GSO sendmsg control message was not observed; trace tail=%s", calls[max(0, len(calls)-1500):])
	}
	t.Logf("original TCP socket survived real UDP hopping; destination packets=%d/%d, source ports=%d; verified payload >=1MiB; UDP_SEGMENT observed", counts[0].Load(), counts[1].Load(), unique.Load())
}
