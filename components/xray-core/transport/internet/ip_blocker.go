package internet

import (
	"context"
	"net/netip"

	"github.com/xtls/xray-core/common/errors"
	"github.com/xtls/xray-core/common/net"
	"github.com/xtls/xray-core/common/session"
)

type ipBlockerKey struct{}
type ipBlocker struct {
	blocked  func(net.IP) bool
	outbound *session.Outbound
}

func currentDialOutbound(ctx context.Context) *session.Outbound {
	outbounds := session.OutboundsFromContext(ctx)
	if len(outbounds) == 0 {
		return nil
	}
	return outbounds[len(outbounds)-1]
}

// ContextWithIPBlocker applies an outbound's policy to the actual socket destination,
// including addresses selected by the system resolver and Happy Eyeballs.
func ContextWithIPBlocker(ctx context.Context, blocked func(net.IP) bool) context.Context {
	return context.WithValue(ctx, ipBlockerKey{}, ipBlocker{blocked: blocked, outbound: currentDialOutbound(ctx)})
}

func checkDialIP(ctx context.Context, ip net.IP) error {
	blocker, ok := ctx.Value(ipBlockerKey{}).(ipBlocker)
	// A dialerProxy's socket belongs to another outbound: its proxy endpoint is not
	// the logical target governed by this policy. Keep the guard scoped to its owner.
	if ok && blocker.outbound == currentDialOutbound(ctx) && blocker.blocked != nil && blocker.blocked(ip) {
		return errors.New("blocked target IP: ", ip)
	}
	return nil
}

func checkDialAddress(ctx context.Context, address string) error {
	// Control receives the numeric address selected by net.Dialer, before connect().
	// Do not resolve a hostname here: validating one answer and dialing another is unsafe.
	endpoint, err := netip.ParseAddrPort(address)
	if err != nil {
		return errors.New("invalid resolved dial address").Base(err)
	}
	return checkDialIP(ctx, net.IP(endpoint.Addr().Unmap().AsSlice()))
}
