//go:build !linux

package tls

import (
	"fmt"
	"net"
)

func preflightKernelTLS() error {
	return fmt.Errorf("kTLS requires Linux")
}

func installKernelTLSTX(net.Conn, *kernelTLSTXKeyMaterial) (bool, error) {
	return false, fmt.Errorf("kTLS requires Linux")
}

func closeKernelTLS(conn net.Conn) error {
	return conn.Close()
}
