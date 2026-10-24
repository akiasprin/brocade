//go:build !linux

package proxy

import (
	"context"
	"net"
)

func copyAccountedSplice(context.Context, net.Conn, *net.TCPConn, func(int64)) (bool, error) {
	return false, nil
}
