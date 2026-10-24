//go:build linux

package tcp

import (
	"context"
	"fmt"
	"net"
	"testing"
	"time"

	"golang.org/x/sys/unix"
)

func runtimeHandoffControl(t *testing.T) (*tcpHandoff, *tcpHandoffBinding, *tcpHandoffBinding) {
	t.Helper()
	alice, bob := handoffTestListener(t), handoffTestListener(t)
	manifest := handoffTestManifest(t, alice)
	manifest.Version, manifest.Scope = 2, "whole-generation"
	manifest.Listeners = []handoffEndpoint{
		{Address: alice.Addr().String(), Network: "tcp", Role: "public", Protocol: unix.IPPROTO_TCP},
		{Address: bob.Addr().String(), Network: "tcp", Role: "public", Protocol: unix.IPPROTO_TCP},
	}
	c := handoffTestControl(t, manifest)
	_, a := handoffTestOpen(t, c, alice)
	_, b := handoffTestOpen(t, c, bob)
	if err := c.markReady(); err != nil {
		t.Fatal(err)
	}
	if err := c.seal(); err != nil {
		t.Fatal(err)
	}
	if err := c.resume(); err != nil {
		t.Fatal(err)
	}
	return c, a.(*tcpHandoffBinding), b.(*tcpHandoffBinding)
}

func requireRuntimeAccept(t *testing.T, b *tcpHandoffBinding) {
	t.Helper()
	accepted := make(chan bool, 1)
	go func() { accepted <- b.beforeAccept() }()
	select {
	case ok := <-accepted:
		if !ok {
			t.Fatal("unrelated public listener stopped accepting")
		}
		b.afterAccept(false)
	case <-time.After(time.Second):
		t.Fatal("unrelated public listener was paused")
	}
}

func TestWholeGenerationRuntimeTCPRevocationAndRegrant(t *testing.T) {
	c, alice, bob := runtimeHandoffControl(t)
	address := alice.endpoint.Address
	if !alice.beforeAccept() {
		t.Fatal("initial admission failed")
	}
	alice.afterAccept(true)
	alice.listener.Close()
	alice.close()
	status := c.status()
	if !status.Ready || !status.Accepting || status.PublicActive != 1 {
		t.Fatalf("revocation paused the group or forgot an existing connection: %+v", status)
	}
	requireRuntimeAccept(t, bob)
	if err := c.pause(context.Background()); err != nil {
		t.Fatal(err)
	}
	if fds, err := c.exportListeners(); err == nil {
		for _, fd := range fds {
			unix.Close(fd)
		}
		t.Fatal("export succeeded before the removed listener drained")
	}
	alice.finishConnection()
	if err := c.resume(); err != nil {
		t.Fatal(err)
	}
	// Closing the old listener twice must not invalidate the serving group.
	alice.close()
	listener, replacement, err := c.open(address, false, nil, func() (net.Listener, error) {
		return net.Listen("tcp4", address)
	})
	if err != nil || replacement == nil {
		t.Fatalf("regrant could not recreate its listener: %v", err)
	}
	defer listener.Close()
	alice.close()
	requireRuntimeAccept(t, bob)
	requireRuntimeAccept(t, replacement.(*tcpHandoffBinding))
	if err := c.pause(context.Background()); err != nil {
		t.Fatal(err)
	}
	fds, err := c.exportListeners()
	if err != nil {
		t.Fatal(err)
	}
	defer func() {
		for _, fd := range fds {
			unix.Close(fd)
		}
	}()
	if len(fds) != 2 || !c.status().RetirementSupported {
		t.Fatal("regrant lost whole-generation handoff coverage")
	}
	if err := c.resume(); err != nil {
		t.Fatal(err)
	}
}

func TestWholeGenerationRuntimeRevocationWaitsForAccept(t *testing.T) {
	for _, accepted := range []bool{false, true} {
		t.Run(fmt.Sprint(accepted), func(t *testing.T) {
			c, alice, bob := runtimeHandoffControl(t)
			if !alice.beforeAccept() {
				t.Fatal("initial Accept failed")
			}
			alice.listener.Close()
			alice.close()
			if len(c.status().Listeners) != 2 {
				t.Fatal("listener was forgotten while Accept could still admit a connection")
			}
			requireRuntimeAccept(t, bob)
			alice.afterAccept(accepted)
			if accepted {
				if status := c.status(); status.PublicActive != 1 || len(status.Listeners) != 2 {
					t.Fatalf("late admission was forgotten: %+v", status)
				}
				alice.finishConnection()
			}
			if status := c.status(); !status.RetirementSupported || len(status.Listeners) != 1 {
				t.Fatalf("completed revocation left the generation unusable: %+v", status)
			}
		})
	}
}

func TestWholeGenerationRuntimeUDPRevocationAndRegrant(t *testing.T) {
	c, _, bob := runtimeHandoffControl(t)
	reserved, err := net.ListenPacket("udp4", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	address := reserved.LocalAddr().String()
	reserved.Close()
	bind := func(address string) (net.PacketConn, error) { return net.ListenPacket("udp4", address) }
	conn, gate, err := c.openPacket(address, nil, bind)
	if err != nil || gate == nil {
		t.Fatalf("runtime UDP listener was not tracked: %v", err)
	}
	if _, ok := gate.BeforeAccept(context.Background()); !ok {
		t.Fatal("initial UDP admission failed")
	}
	gate.AfterAccept(true)
	conn.Close()
	gate.Close()
	if status := c.status(); !status.Ready || !status.Accepting || status.PublicActive != 1 {
		t.Fatalf("UDP revocation paused TCP or forgot admitted traffic: %+v", status)
	}
	requireRuntimeAccept(t, bob)
	gate.FinishConnection()
	gate.Close()
	replacement, newGate, err := c.openPacket(address, nil, bind)
	if err != nil || newGate == nil {
		t.Fatalf("runtime UDP regrant failed: %v", err)
	}
	defer replacement.Close()
	if status := c.status(); !status.RetirementSupported || len(status.Listeners) != 3 {
		t.Fatalf("runtime UDP membership lost handoff coverage: %+v", status)
	}
	replacement.Close()
	newGate.Close()
	if status := c.status(); !status.RetirementSupported || len(status.Listeners) != 2 {
		t.Fatalf("empty revoked UDP listener was retained: %+v", status)
	}
}

func TestWholeGenerationRuntimeFailedBindPreservesMembership(t *testing.T) {
	c, _, bob := runtimeHandoffControl(t)
	before := len(c.status().Listeners)
	if _, _, err := c.open("127.0.0.1:55555", false, nil, func() (net.Listener, error) {
		return nil, fmt.Errorf("injected bind failure")
	}); err == nil {
		t.Fatal("bind failure was hidden")
	}
	requireRuntimeAccept(t, bob)
	if status := c.status(); len(status.Listeners) != before || !status.RetirementSupported {
		t.Fatalf("failed bind changed the serving contract: %+v", status)
	}
}
