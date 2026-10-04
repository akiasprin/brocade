//go:build !linux

package anytls

import (
	"net"

	"github.com/xtls/xray-core/common/buf"
)

type kernelTLSWritevResult struct {
	handled      bool
	payloadBytes int64
	wireBytes    int64
	batches      int64
	syscalls     int64
	err          error
}

func writePSHBatchKernelTLS(net.Conn, uint32, buf.MultiBuffer) kernelTLSWritevResult {
	return kernelTLSWritevResult{}
}

func kernelTLSWritevAvailable(net.Conn) bool {
	return false
}
