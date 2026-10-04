//go:build !linux

package tls

import (
	"fmt"
	"net"
)

func preflightKernelTLS() error {
	return fmt.Errorf("kTLS requires Linux")
}

func installKernelTLS(net.Conn, *kernelTLSKeyMaterial) (bool, error) {
	return false, fmt.Errorf("kTLS requires Linux")
}

func newKernelTLSConn(conn net.Conn) net.Conn {
	return conn
}

func closeKernelTLS(conn net.Conn) error {
	return conn.Close()
}
