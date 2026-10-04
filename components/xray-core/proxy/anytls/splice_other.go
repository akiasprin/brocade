//go:build !linux

package anytls

import (
	"context"
	"net"
)

type framedDownlinkSplicer struct {
	session   *session
	sid       uint32
	ready     chan struct{}
	useSplice bool
}

func (*framedDownlinkSplicer) SpliceDownlink(context.Context, net.Conn, func(int64)) (bool, error) {
	return false, nil
}
