package freedom

import (
	"testing"

	"github.com/xtls/xray-core/common/session"
)

func TestLegacyDisableStillAllowsOnlyVisionSplice(t *testing.T) {
	tests := []struct {
		name    string
		inbound *session.Inbound
		want    bool
	}{
		{name: "missing inbound", inbound: nil, want: false},
		{name: "raw dokodemo", inbound: spliceInbound("dokodemo-door", session.SpliceCopyDirect), want: false},
		{name: "raw VLESS", inbound: spliceInbound("vless", session.SpliceCopyDisabled), want: false},
		{name: "Vision waiting", inbound: spliceInbound("vless", session.SpliceCopyWaiting), want: true},
		{name: "Vision direct", inbound: spliceInbound("vless", session.SpliceCopyDirect), want: true},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			if got := responseSpliceAllowed(false, test.inbound); got != test.want {
				t.Fatalf("responseSpliceAllowed() = %v, want %v", got, test.want)
			}
		})
	}
}

func TestEnabledSpliceKeepsOrdinaryRawConnections(t *testing.T) {
	if !responseSpliceAllowed(true, spliceInbound("dokodemo-door", session.SpliceCopyDirect)) {
		t.Fatal("enabled splice rejected an ordinary raw connection")
	}
}

func spliceInbound(name string, state session.SpliceCopyState) *session.Inbound {
	inbound := &session.Inbound{Name: name}
	inbound.CanSpliceCopy.Store(state)
	return inbound
}
