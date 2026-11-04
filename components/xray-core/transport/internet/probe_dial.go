package internet

import (
	"context"
	stderrors "errors"
	stdnet "net"
	"os"
	"syscall"

	"github.com/xtls/xray-core/common/net"
	"github.com/xtls/xray-core/features/probetrace"
)

// ProbeDialError retains the system dial's cause while attaching a bounded diagnostic
// category. Ordinary connections never receive this wrapper.
type ProbeDialError struct {
	Code  string
	Cause error
}

func (e *ProbeDialError) Error() string { return e.Cause.Error() }
func (e *ProbeDialError) Unwrap() error { return e.Cause }

func ProbeDialErrorCode(err error) string {
	var failure *ProbeDialError
	if stderrors.As(err, &failure) {
		return failure.Code
	}
	return "connect_error"
}

func probeDialError(ctx context.Context, src net.Address, dest net.Destination, sockopt *SocketConfig, err error) error {
	if err == nil || probetrace.FromContext(ctx) == nil || dest.Network != net.Network_TCP || !dest.Address.Family().IsIP() {
		return err
	}
	checkSources := func() (bool, error) { return usableIPv6Source(dest) }
	// Transparent/external socket controllers can select sources outside the normal
	// interface inventory. Their failures remain specific without asserting absence.
	ControllersLock.Lock()
	externalControllers := len(Controllers) != 0
	ControllersLock.Unlock()
	if externalControllers || sockopt != nil && sockopt.Tproxy != SocketConfig_Off {
		checkSources = func() (bool, error) { return false, os.ErrInvalid }
	}
	return &ProbeDialError{Code: probeDialCode(src, dest, err, checkSources), Cause: err}
}

func probeDialCode(src net.Address, dest net.Destination, err error, hasIPv6 func() (bool, error)) string {
	ipv6 := dest.Address.Family().IsIPv6()
	if !wildcardProbeSource(src) && src.Family().IsIP() && src.Family() != dest.Address.Family() {
		return "source_family_mismatch"
	}
	var operation *os.SyscallError
	stderrors.As(err, &operation)
	if ipv6 && operation != nil && operation.Syscall == "socket" &&
		(stderrors.Is(err, syscall.EAFNOSUPPORT) || stderrors.Is(err, syscall.EPROTONOSUPPORT)) {
		return "ipv6_not_supported"
	}
	if stderrors.Is(err, syscall.ENETUNREACH) {
		if ipv6 && dest.Address.IP().IsGlobalUnicast() && wildcardProbeSource(src) {
			usable, inventoryErr := hasIPv6()
			if inventoryErr == nil && !usable {
				return "ipv6_no_address"
			}
		}
		return "network_unreachable"
	}
	if stderrors.Is(err, syscall.EHOSTUNREACH) {
		return "host_unreachable"
	}
	if stderrors.Is(err, syscall.ECONNREFUSED) {
		return "connection_refused"
	}
	if stderrors.Is(err, syscall.EADDRNOTAVAIL) && operation != nil && operation.Syscall == "bind" {
		return "source_unavailable"
	}
	if stderrors.Is(err, syscall.EACCES) || stderrors.Is(err, syscall.EPERM) {
		return "permission_denied"
	}
	var timeout stdnet.Error
	if stderrors.As(err, &timeout) && timeout.Timeout() {
		return "connect_timeout"
	}
	if stderrors.Is(err, syscall.ECONNRESET) {
		return "connection_reset"
	}
	return "connect_error"
}

func wildcardProbeSource(source net.Address) bool {
	return source == nil || source.Family().IsIP() && source.IP().IsUnspecified()
}

func usableIPv6Source(dest net.Destination) (bool, error) {
	interfaces, err := stdnet.Interfaces()
	if err != nil {
		return false, err
	}
	var inventoryErr error
	for _, iface := range interfaces {
		if iface.Flags&stdnet.FlagUp == 0 {
			continue
		}
		addresses, err := iface.Addrs()
		if err != nil {
			inventoryErr = err
			continue
		}
		usable, err := probeIPv6Sources(addresses, func() (bool, error) {
			// UDP connect asks the kernel to select a route/source without sending packets.
			// ULA alone (e.g. the Brocade overlay) does not establish Internet egress;
			// a routed ULA source still supports NAT66 or a gateway on another host.
			conn, err := stdnet.DialUDP("udp6", nil, &stdnet.UDPAddr{IP: dest.Address.IP(), Port: int(dest.Port)})
			if err != nil {
				if stderrors.Is(err, syscall.ENETUNREACH) {
					return false, nil
				}
				return false, err
			}
			defer conn.Close()
			return conn.LocalAddr().(*stdnet.UDPAddr).IP.IsGlobalUnicast(), nil
		})
		if usable {
			return true, nil
		}
		if err != nil {
			inventoryErr = err
		}
	}
	return false, inventoryErr
}

func probeIPv6Sources(addresses []stdnet.Addr, routedULA func() (bool, error)) (bool, error) {
	private := false
	for _, address := range addresses {
		ip, _, err := stdnet.ParseCIDR(address.String())
		if err != nil {
			return false, err
		}
		if ip.To4() == nil && ip.IsGlobalUnicast() {
			if !ip.IsPrivate() {
				// A configured global source with a broken route stays a network failure.
				return true, nil
			}
			private = true
		}
	}
	if private {
		return routedULA()
	}
	return false, nil
}
