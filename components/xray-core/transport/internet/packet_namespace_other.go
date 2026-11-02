//go:build !linux

package internet

import (
	"context"
	"fmt"
	"net"
)

func listenPacketNamespace(ctx context.Context, addr net.Addr, config *SocketConfig, _ string, policy string) (net.PacketConn, error) {
	if policy != "" {
		return nil, fmt.Errorf("inbound UDP network namespaces require Linux")
	}
	return ListenSystemPacket(ctx, addr, config)
}
