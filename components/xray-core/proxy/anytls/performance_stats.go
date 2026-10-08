package anytls

import (
	"github.com/xtls/xray-core/features/stats"
	v2tls "github.com/xtls/xray-core/transport/internet/tls"
)

const performanceStatsPrefix = "anytls>>>performance>>>"

type performanceStats struct {
	kernelTLSConnections                  stats.Counter
	kernelTLSActive                       stats.Counter
	kernelTLSFallback                     stats.Counter
	kernelTLSFallbackReasons              map[v2tls.KernelTLSFallbackReason]stats.Counter
	kernelTLSAuthenticatedConnections     stats.Counter
	kernelTLSAuthenticatedActive          stats.Counter
	kernelTLSAuthenticatedFallback        stats.Counter
	kernelTLSAuthenticatedFallbackReasons map[v2tls.KernelTLSFallbackReason]stats.Counter
	spliceConnections                     stats.Counter
	spliceBytes                           stats.Counter
	spliceErrors                          stats.Counter
	writevBatches                         stats.Counter
	writevBytes                           stats.Counter
	writevSyscalls                        stats.Counter
	writevErrors                          stats.Counter
}

var observedKernelTLSFallbackReasons = []v2tls.KernelTLSFallbackReason{
	v2tls.KernelTLSFallbackDisabled,
	v2tls.KernelTLSFallbackUnsupportedConnection,
	v2tls.KernelTLSFallbackNotServer,
	v2tls.KernelTLSFallbackApplicationIO,
	v2tls.KernelTLSFallbackPreflight,
	v2tls.KernelTLSFallbackSessionTickets,
	v2tls.KernelTLSFallbackTLSVersion,
	v2tls.KernelTLSFallbackTrafficSecrets,
	v2tls.KernelTLSFallbackCipherSuite,
	v2tls.KernelTLSFallbackSocket,
	v2tls.KernelTLSFallbackOther,
}

func newPerformanceStats(manager stats.Manager) *performanceStats {
	performance := &performanceStats{
		kernelTLSConnections:                  getOrRegisterPerformanceCounter(manager, performanceStatsPrefix+"ktls_connections"),
		kernelTLSActive:                       getOrRegisterPerformanceCounter(manager, performanceStatsPrefix+"ktls_active_connections"),
		kernelTLSFallback:                     getOrRegisterPerformanceCounter(manager, performanceStatsPrefix+"ktls_fallback_connections"),
		kernelTLSFallbackReasons:              make(map[v2tls.KernelTLSFallbackReason]stats.Counter),
		kernelTLSAuthenticatedConnections:     getOrRegisterPerformanceCounter(manager, performanceStatsPrefix+"ktls_authenticated_connections"),
		kernelTLSAuthenticatedActive:          getOrRegisterPerformanceCounter(manager, performanceStatsPrefix+"ktls_authenticated_active_connections"),
		kernelTLSAuthenticatedFallback:        getOrRegisterPerformanceCounter(manager, performanceStatsPrefix+"ktls_authenticated_fallback_connections"),
		kernelTLSAuthenticatedFallbackReasons: make(map[v2tls.KernelTLSFallbackReason]stats.Counter),
		spliceConnections:                     getOrRegisterPerformanceCounter(manager, performanceStatsPrefix+"splice_connections"),
		spliceBytes:                           getOrRegisterPerformanceCounter(manager, performanceStatsPrefix+"splice_bytes"),
		spliceErrors:                          getOrRegisterPerformanceCounter(manager, performanceStatsPrefix+"splice_errors"),
		writevBatches:                         getOrRegisterPerformanceCounter(manager, performanceStatsPrefix+"writev_batches"),
		writevBytes:                           getOrRegisterPerformanceCounter(manager, performanceStatsPrefix+"writev_bytes"),
		writevSyscalls:                        getOrRegisterPerformanceCounter(manager, performanceStatsPrefix+"writev_syscalls"),
		writevErrors:                          getOrRegisterPerformanceCounter(manager, performanceStatsPrefix+"writev_errors"),
	}
	for _, reason := range observedKernelTLSFallbackReasons {
		performance.kernelTLSFallbackReasons[reason] = getOrRegisterPerformanceCounter(
			manager,
			performanceStatsPrefix+"ktls_fallback_reason_"+string(reason)+"_connections",
		)
		performance.kernelTLSAuthenticatedFallbackReasons[reason] = getOrRegisterPerformanceCounter(
			manager,
			performanceStatsPrefix+"ktls_authenticated_fallback_reason_"+string(reason)+"_connections",
		)
	}
	return performance
}

func getOrRegisterPerformanceCounter(manager stats.Manager, name string) stats.Counter {
	if manager == nil {
		return nil
	}
	if counter := manager.GetCounter(name); counter != nil {
		return counter
	}
	counter, err := manager.RegisterCounter(name)
	if err == nil {
		return counter
	}
	return manager.GetCounter(name)
}

// recordKernelTLS counts every TLS handshake reaching the public listener,
// including scanners that never authenticate as AnyTLS.
func (s *performanceStats) recordKernelTLS(active bool, reason v2tls.KernelTLSFallbackReason) {
	if s == nil {
		return
	}
	recordKernelTLSCounters(
		s.kernelTLSConnections,
		s.kernelTLSActive,
		s.kernelTLSFallback,
		s.kernelTLSFallbackReasons,
		active,
		reason,
	)
}

// recordAuthenticatedKernelTLS is the client-compatibility signal: it is only
// called after the AnyTLS password and initial padding have been accepted.
func (s *performanceStats) recordAuthenticatedKernelTLS(active bool, reason v2tls.KernelTLSFallbackReason) {
	if s == nil {
		return
	}
	recordKernelTLSCounters(
		s.kernelTLSAuthenticatedConnections,
		s.kernelTLSAuthenticatedActive,
		s.kernelTLSAuthenticatedFallback,
		s.kernelTLSAuthenticatedFallbackReasons,
		active,
		reason,
	)
}

func recordKernelTLSCounters(
	connections stats.Counter,
	activeConnections stats.Counter,
	fallbackConnections stats.Counter,
	fallbackReasons map[v2tls.KernelTLSFallbackReason]stats.Counter,
	active bool,
	reason v2tls.KernelTLSFallbackReason,
) {
	if connections != nil {
		connections.Add(1)
	}
	if active {
		if activeConnections != nil {
			activeConnections.Add(1)
		}
		return
	}
	if fallbackConnections != nil {
		fallbackConnections.Add(1)
	}
	if reason == v2tls.KernelTLSFallbackNone {
		reason = v2tls.KernelTLSFallbackOther
	}
	if counter := fallbackReasons[reason]; counter != nil {
		counter.Add(1)
	} else if counter := fallbackReasons[v2tls.KernelTLSFallbackOther]; counter != nil {
		counter.Add(1)
	}
}

func (s *performanceStats) recordSpliceConnection() {
	if s != nil && s.spliceConnections != nil {
		s.spliceConnections.Add(1)
	}
}

func (s *performanceStats) recordSpliceBytes(bytes int64) {
	if s != nil && bytes > 0 && s.spliceBytes != nil {
		s.spliceBytes.Add(bytes)
	}
}

func (s *performanceStats) recordSpliceError() {
	if s != nil && s.spliceErrors != nil {
		s.spliceErrors.Add(1)
	}
}

func (s *performanceStats) recordWritev(payloadBytes, batches, syscalls int64) {
	if s == nil {
		return
	}
	if batches > 0 && s.writevBatches != nil {
		s.writevBatches.Add(batches)
	}
	if payloadBytes > 0 && s.writevBytes != nil {
		s.writevBytes.Add(payloadBytes)
	}
	if syscalls > 0 && s.writevSyscalls != nil {
		s.writevSyscalls.Add(syscalls)
	}
}

func (s *performanceStats) recordWritevError(syscalls int64) {
	if s == nil {
		return
	}
	if syscalls > 0 && s.writevSyscalls != nil {
		s.writevSyscalls.Add(syscalls)
	}
	if s.writevErrors != nil {
		s.writevErrors.Add(1)
	}
}
