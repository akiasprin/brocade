package anytls

import (
	"strings"

	"github.com/xtls/xray-core/common/platform"
)

// framedDownlinkSpliceEnabled is deliberately opt-in. AnyTLS must interleave a
// seven-byte header with every 64 KiB payload, so its pipe-based path needs
// separate header writes and depends heavily on kernel pipe/kTLS behaviour.
// It can win peak throughput on a suitable kernel, but has a less predictable
// syscall, latency, and memory profile than the writev path. Keep kTLS itself
// independent: xray.anytls.ktls=auto can still move transmit AES-GCM into the
// kernel without enabling this framing experiment.
func framedDownlinkSpliceEnabled() bool {
	value := platform.NewEnvFlag(platform.UseAnyTLSSplice).GetValue(func() string {
		return "off"
	})
	switch strings.ToLower(strings.TrimSpace(value)) {
	case "1", "true", "on", "enable", "enabled":
		return true
	default:
		return false
	}
}

// kernelTLSVectoredWriteEnabled controls the copy-free userspace framing path.
// Unlike pipe-based splice, writev can submit headers and existing payload
// buffers to a software-kTLS socket in one operation. It is safe to default to
// auto because unsupported transports are detected before any bytes are sent.
func kernelTLSVectoredWriteEnabled() bool {
	value := platform.NewEnvFlag(platform.UseAnyTLSWritev).GetValue(func() string {
		return "auto"
	})
	switch strings.ToLower(strings.TrimSpace(value)) {
	case "0", "false", "off", "disable", "disabled":
		return false
	case "", "1", "true", "on", "enable", "enabled", "auto":
		return true
	default:
		return false
	}
}
