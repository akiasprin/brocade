//go:build linux

package tcp

import (
	"context"
	"net"
	"testing"
	"time"
)

func TestPacketHandoffParentCancellationWhilePaused(t *testing.T) {
	control, _, _ := runtimeHandoffControl(t)
	socket, err := net.ListenPacket("udp4", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	address := socket.LocalAddr().String()
	_ = socket.Close()
	conn, gate, err := control.openPacket(address, nil, func(address string) (net.PacketConn, error) { return net.ListenPacket("udp4", address) })
	if err != nil {
		t.Fatal(err)
	}
	defer conn.Close()
	if err := control.pause(context.Background()); err != nil {
		t.Fatal(err)
	}
	parent, cancel := context.WithCancel(context.Background())
	defer cancel()
	done := make(chan bool, 1)
	go func() { _, admitted := gate.BeforeAccept(parent); done <- admitted }()
	cancel()
	select {
	case admitted := <-done:
		if admitted {
			t.Fatal("canceled parent admitted an accept")
		}
	case <-time.After(time.Second):
		t.Fatal("canceled HTTP/3 server context remained blocked on a paused generation")
	}
}
