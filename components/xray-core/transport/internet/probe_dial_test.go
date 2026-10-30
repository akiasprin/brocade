package internet

import (
	"context"
	"errors"
	"fmt"
	stdnet "net"
	"os"
	"syscall"
	"testing"
	"time"

	"github.com/xtls/xray-core/common/net"
	"github.com/xtls/xray-core/features/probetrace"
)

func TestProbeDialClassificationRequiresActualIPv6Absence(t *testing.T) {
	v6 := net.TCPDestination(net.ParseAddress("2001:db8::1"), 443)
	v4 := net.TCPDestination(net.ParseAddress("192.0.2.1"), 443)
	socketError := func(operation string, errno syscall.Errno) error {
		return &stdnet.OpError{Op: "dial", Net: "tcp", Err: &os.SyscallError{Syscall: operation, Err: errno}}
	}
	tests := []struct {
		name         string
		dest         net.Destination
		src          net.Address
		err          error
		usable       bool
		inventoryErr error
		want         string
	}{
		{name: "no IPv6 source", dest: v6, err: socketError("connect", syscall.ENETUNREACH), want: "ipv6_no_address"},
		{name: "unspecified IPv6 source", dest: v6, src: net.ParseAddress("::"), err: socketError("connect", syscall.ENETUNREACH), want: "ipv6_no_address"},
		{name: "unspecified IPv4 source", dest: v6, src: net.ParseAddress("0.0.0.0"), err: socketError("connect", syscall.ENETUNREACH), want: "ipv6_no_address"},
		{name: "IPv6 route failure with a source", dest: v6, err: socketError("connect", syscall.ENETUNREACH), usable: true, want: "network_unreachable"},
		{name: "inventory failure is not absence", dest: v6, err: socketError("connect", syscall.ENETUNREACH), inventoryErr: os.ErrPermission, want: "network_unreachable"},
		{name: "IPv4 absence is not IPv6 absence", dest: v4, err: socketError("connect", syscall.ENETUNREACH), want: "network_unreachable"},
		{name: "IPv6 socket unavailable", dest: v6, err: socketError("socket", syscall.EAFNOSUPPORT), want: "ipv6_not_supported"},
		{name: "host unreachable does not prove global absence", dest: v6, err: socketError("connect", syscall.EHOSTUNREACH), want: "host_unreachable"},
		{name: "refused is not absence", dest: v6, err: socketError("connect", syscall.ECONNREFUSED), want: "connection_refused"},
		{name: "timeout is not absence", dest: v6, err: socketError("connect", syscall.ETIMEDOUT), want: "connect_timeout"},
		{name: "bound source missing", dest: v6, err: socketError("bind", syscall.EADDRNOTAVAIL), want: "source_unavailable"},
		{name: "ephemeral ports exhausted is not a missing source", dest: v6, err: socketError("connect", syscall.EADDRNOTAVAIL), want: "connect_error"},
		{name: "source family mismatch", dest: v6, src: net.ParseAddress("192.0.2.2"), err: socketError("connect", syscall.ENETUNREACH), want: "source_family_mismatch"},
		{name: "explicit nonlocal source does not establish host absence", dest: v6, src: net.ParseAddress("2001:db8::2"), err: socketError("connect", syscall.ENETUNREACH), want: "network_unreachable"},
		{name: "link local target does not establish global absence", dest: net.TCPDestination(net.ParseAddress("fe80::1"), 443), err: socketError("connect", syscall.ENETUNREACH), want: "network_unreachable"},
		{name: "local policy denied", dest: v6, err: socketError("connect", syscall.EACCES), want: "permission_denied"},
		{name: "reset", dest: v6, err: socketError("connect", syscall.ECONNRESET), want: "connection_reset"},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			got := probeDialCode(test.src, test.dest, fmt.Errorf("outer: %w", test.err), func() (bool, error) { return test.usable, test.inventoryErr })
			if got != test.want {
				t.Fatalf("code=%q, want %q", got, test.want)
			}
		})
	}
}

func TestProbeDialOrdinaryTrafficAndWrappedCauseStayIntact(t *testing.T) {
	dest := net.TCPDestination(net.ParseAddress("2001:db8::1"), 443)
	cause := &os.SyscallError{Syscall: "connect", Err: syscall.ENETUNREACH}
	if got := probeDialError(context.Background(), nil, dest, nil, cause); got != cause {
		t.Fatal("ordinary error was replaced")
	}
	wrapped := &ProbeDialError{Code: "network_unreachable", Cause: cause}
	if !errors.Is(wrapped, syscall.ENETUNREACH) || ProbeDialErrorCode(fmt.Errorf("wrapped: %w", wrapped)) != "network_unreachable" {
		t.Fatal("structured error lost its cause/category")
	}
	if ProbeDialErrorCode(cause) != "connect_error" {
		t.Fatal("an unattributed raw error was classified as an exit result")
	}
}

func TestProbeDialDockerNetwork(t *testing.T) {
	mode := os.Getenv("BROCADE_PROBE_DIAL_DOCKER")
	if mode == "" {
		t.Skip("requires isolated Docker network namespace")
	}
	ctx, cancel := context.WithTimeout(probetrace.WithState(context.Background(), &probetrace.State{}), time.Second)
	defer cancel()
	d := &DefaultSystemDialer{}
	dest := net.TCPDestination(net.ParseAddress("2606:4700:4700::1111"), 443)
	conn, err := d.Dial(ctx, nil, dest, nil)
	if conn != nil {
		conn.Close()
		t.Fatal("isolated network unexpectedly connected")
	}
	want := "ipv6_no_address"
	if mode == "address-without-route" {
		want = "network_unreachable"
	}
	if got := ProbeDialErrorCode(err); got != want {
		t.Fatalf("category=%q want=%q error=%v", got, want, err)
	}
	listener, err := stdnet.Listen("tcp6", "[::1]:0")
	if err != nil {
		t.Fatal(err)
	}
	defer listener.Close()
	dest = net.TCPDestination(net.ParseAddress("::1"), net.Port(listener.Addr().(*stdnet.TCPAddr).Port))
	conn, err = d.Dial(ctx, nil, dest, nil)
	if err != nil {
		t.Fatalf("working IPv6 loopback was preempted by host observations: %v", err)
	}
	conn.Close()
}
