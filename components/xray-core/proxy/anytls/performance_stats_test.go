package anytls

import (
	"sync/atomic"
	"testing"

	"github.com/xtls/xray-core/features/stats"
	v2tls "github.com/xtls/xray-core/transport/internet/tls"
)

type performanceTestCounter struct {
	value atomic.Int64
}

func (c *performanceTestCounter) Value() int64 {
	return c.value.Load()
}

func (c *performanceTestCounter) Set(value int64) int64 {
	return c.value.Swap(value)
}

func (c *performanceTestCounter) Add(value int64) int64 {
	return c.value.Add(value)
}

func TestKernelTLSStatsSeparateHandshakeAndAuthenticatedTraffic(t *testing.T) {
	handshakes := &performanceTestCounter{}
	handshakeActive := &performanceTestCounter{}
	handshakeFallback := &performanceTestCounter{}
	handshakeTLSVersion := &performanceTestCounter{}
	handshakeOther := &performanceTestCounter{}
	authenticated := &performanceTestCounter{}
	authenticatedActive := &performanceTestCounter{}
	authenticatedFallback := &performanceTestCounter{}
	authenticatedOther := &performanceTestCounter{}
	performance := &performanceStats{
		kernelTLSConnections:              handshakes,
		kernelTLSActive:                   handshakeActive,
		kernelTLSFallback:                 handshakeFallback,
		kernelTLSFallbackReasons:          map[v2tls.KernelTLSFallbackReason]stats.Counter{},
		kernelTLSAuthenticatedConnections: authenticated,
		kernelTLSAuthenticatedActive:      authenticatedActive,
		kernelTLSAuthenticatedFallback:    authenticatedFallback,
		kernelTLSAuthenticatedFallbackReasons: map[v2tls.KernelTLSFallbackReason]stats.Counter{
			v2tls.KernelTLSFallbackOther: authenticatedOther,
		},
	}
	performance.kernelTLSFallbackReasons[v2tls.KernelTLSFallbackTLSVersion] = handshakeTLSVersion
	performance.kernelTLSFallbackReasons[v2tls.KernelTLSFallbackOther] = handshakeOther

	// A public TLS scanner can negotiate TLS 1.2 but never authenticate as AnyTLS.
	performance.recordKernelTLS(false, v2tls.KernelTLSFallbackTLSVersion)
	if handshakes.Value() != 1 || handshakeFallback.Value() != 1 || handshakeTLSVersion.Value() != 1 {
		t.Fatalf("handshake counters = total:%d fallback:%d tls-version:%d", handshakes.Value(), handshakeFallback.Value(), handshakeTLSVersion.Value())
	}
	if authenticated.Value() != 0 || authenticatedFallback.Value() != 0 {
		t.Fatalf("scanner changed authenticated counters = total:%d fallback:%d", authenticated.Value(), authenticatedFallback.Value())
	}

	performance.recordKernelTLS(true, v2tls.KernelTLSFallbackNone)
	performance.recordAuthenticatedKernelTLS(true, v2tls.KernelTLSFallbackNone)
	if handshakeActive.Value() != 1 || authenticated.Value() != 1 || authenticatedActive.Value() != 1 {
		t.Fatalf("active counters = handshake:%d authenticated-total:%d authenticated-active:%d", handshakeActive.Value(), authenticated.Value(), authenticatedActive.Value())
	}

	performance.recordAuthenticatedKernelTLS(false, v2tls.KernelTLSFallbackReason("future_reason"))
	if authenticatedFallback.Value() != 1 || authenticatedOther.Value() != 1 {
		t.Fatalf("unknown fallback counters = fallback:%d other:%d", authenticatedFallback.Value(), authenticatedOther.Value())
	}
}
