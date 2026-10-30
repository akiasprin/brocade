package internet

import (
	"context"
	"fmt"
	"net"
	"sync"
)

// PacketHandoffGate is the admission and lifetime boundary for connection-oriented and
// session-oriented protocols carried by a UDP socket. Pausing admission never closes the
// PacketConn: the old generation must keep receiving packets for its established flows.
type PacketHandoffGate interface {
	BeforeAccept(context.Context) (context.Context, bool)
	AfterAccept(bool)
	StartConnection() bool
	FinishConnection()
	Close()
}

type packetHandoffController func(string, *SocketConfig, func(string) (net.PacketConn, error)) (net.PacketConn, PacketHandoffGate, error)

var packetHandoffRegistry struct {
	sync.RWMutex
	controller packetHandoffController
}

// RegisterPacketHandoffController installs the process-wide inbound UDP lifecycle controller.
// The returned closure removes only this registration and is safe to call more than once.
func RegisterPacketHandoffController(controller packetHandoffController) (func(), error) {
	if controller == nil {
		return nil, fmt.Errorf("packet handoff controller is nil")
	}
	packetHandoffRegistry.Lock()
	defer packetHandoffRegistry.Unlock()
	if packetHandoffRegistry.controller != nil {
		return nil, fmt.Errorf("packet handoff controller is already registered")
	}
	packetHandoffRegistry.controller = controller
	var once sync.Once
	return func() {
		once.Do(func() {
			packetHandoffRegistry.Lock()
			packetHandoffRegistry.controller = nil
			packetHandoffRegistry.Unlock()
		})
	}, nil
}

// OpenPacketHandoffListener delegates an inbound UDP bind when a generation manifest is active,
// and otherwise preserves the ordinary direct bind behavior.
func OpenPacketHandoffListener(logicalAddress string, config *SocketConfig, bind func(string) (net.PacketConn, error)) (net.PacketConn, PacketHandoffGate, error) {
	packetHandoffRegistry.RLock()
	controller := packetHandoffRegistry.controller
	packetHandoffRegistry.RUnlock()
	if controller == nil {
		conn, err := bind(logicalAddress)
		return conn, nil, err
	}
	return controller(logicalAddress, config, bind)
}

// ListenPacketWithHandoff registers the raw socket before applying UDP masks.
// Generation identity and admission belong to that socket, while the transport
// must receive the masked connection to preserve its wire protocol.
func ListenPacketWithHandoff(ctx context.Context, addr *net.UDPAddr, settings *MemoryStreamConfig) (net.PacketConn, PacketHandoffGate, error) {
	var config *SocketConfig
	if settings != nil {
		config = settings.SocketSettings
	}
	packetHandoffRegistry.RLock()
	controller := packetHandoffRegistry.controller
	packetHandoffRegistry.RUnlock()

	var raw net.PacketConn
	var gate PacketHandoffGate
	bound := false
	listenPacket := func(ctx context.Context, addr net.Addr) (net.PacketConn, error) {
		if controller == nil {
			return ListenSystemPacket(ctx, addr, config)
		}
		if bound {
			return nil, fmt.Errorf("UDP handoff cannot manage multiple sockets for one masked listener")
		}
		bound = true
		var err error
		raw, gate, err = controller(addr.String(), config, func(bindAddress string) (net.PacketConn, error) {
			udpAddress, err := net.ResolveUDPAddr("udp", bindAddress)
			if err != nil {
				return nil, err
			}
			return ListenSystemPacket(ctx, udpAddress, config)
		})
		return raw, err
	}

	var conn net.PacketConn
	var err error
	if settings != nil && settings.FinalMask != nil {
		conn, err = settings.FinalMask.WithPacketListener(listenPacket).ListenPacket(ctx, addr)
	} else {
		conn, err = listenPacket(ctx, addr)
	}
	if err == nil && controller != nil && !bound {
		err = fmt.Errorf("UDP mask does not expose a raw socket for handoff")
	}
	if err != nil {
		if conn != nil {
			conn.Close()
		}
		if raw != nil {
			raw.Close()
		}
		if gate != nil {
			gate.Close()
		}
		return nil, nil, err
	}
	return conn, gate, nil
}
