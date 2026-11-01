package internet

import (
	"context"
	"net"
	"testing"

	"golang.org/x/sys/unix"
)

func listenerProtocol(t *testing.T, listener net.Listener) int {
	t.Helper()
	raw, err := listener.(*net.TCPListener).SyscallConn()
	if err != nil {
		t.Fatal(err)
	}
	var protocol int
	var optionErr error
	err = raw.Control(func(fd uintptr) {
		protocol, optionErr = unix.GetsockoptInt(int(fd), unix.SOL_SOCKET, unix.SO_PROTOCOL)
	})
	if err != nil || optionErr != nil {
		t.Fatalf("read listener protocol: raw=%v option=%v", err, optionErr)
	}
	return protocol
}

func TestDefaultListenerDisablesImplicitMPTCP(t *testing.T) {
	// A Go default or inherited runtime setting must not opt proxy listeners into MPTCP.
	t.Setenv("GODEBUG", "multipathtcp=1")
	for _, address := range []string{"127.0.0.1:0", "[::1]:0"} {
		for _, settings := range []struct {
			name   string
			config *SocketConfig
		}{
			{"absent", nil},
			{"empty", &SocketConfig{}},
			{"disabled", &SocketConfig{TcpMptcp: false}},
		} {
			t.Run(address+"/"+settings.name, func(t *testing.T) {
				addr, err := net.ResolveTCPAddr("tcp", address)
				if err != nil {
					t.Fatal(err)
				}
				listener, err := (&DefaultListener{}).Listen(context.Background(), addr, settings.config)
				if err != nil {
					t.Fatal(err)
				}
				defer listener.Close()
				if protocol := listenerProtocol(t, listener); protocol != unix.IPPROTO_TCP {
					t.Fatalf("implicit MPTCP listener exposed: SO_PROTOCOL=%d", protocol)
				}
			})
		}
	}
}

func TestDefaultListenerPreservesExplicitMPTCP(t *testing.T) {
	t.Setenv("GODEBUG", "multipathtcp=0")
	var control net.ListenConfig
	control.SetMultipathTCP(true)
	baseline, err := control.Listen(context.Background(), "tcp4", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	protocol := listenerProtocol(t, baseline)
	baseline.Close()
	if protocol != unix.IPPROTO_MPTCP {
		t.Skip("kernel cannot create MPTCP listeners; implicit-disable cases remain mandatory")
	}
	listener, err := (&DefaultListener{}).Listen(context.Background(), &net.TCPAddr{IP: net.IPv4(127, 0, 0, 1)}, &SocketConfig{TcpMptcp: true})
	if err != nil {
		t.Fatal(err)
	}
	defer listener.Close()
	if protocol := listenerProtocol(t, listener); protocol != unix.IPPROTO_MPTCP {
		t.Fatalf("explicit MPTCP request ignored: SO_PROTOCOL=%d", protocol)
	}
}
