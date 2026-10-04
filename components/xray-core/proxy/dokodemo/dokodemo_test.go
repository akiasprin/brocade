package dokodemo

import (
	gotls "crypto/tls"
	"net"
	"testing"

	"github.com/xtls/xray-core/transport/internet/stat"
	xtls "github.com/xtls/xray-core/transport/internet/tls"
)

func TestSpliceCopyStateAllowsRawTCP(t *testing.T) {
	raw := &net.TCPConn{}
	if got := spliceCopyState(raw); got != 1 {
		t.Fatalf("raw TCP splice state = %d, want 1", got)
	}

	counted := &stat.CounterConnection{Connection: raw}
	if got := spliceCopyState(counted); got != 1 {
		t.Fatalf("counted raw TCP splice state = %d, want 1", got)
	}
}

func TestSpliceCopyStateRejectsSecurityWrapper(t *testing.T) {
	raw := &net.TCPConn{}
	secured := xtls.Server(raw, &gotls.Config{})
	if got := spliceCopyState(secured); got != 3 {
		t.Fatalf("TLS-wrapped TCP splice state = %d, want 3", got)
	}
}
