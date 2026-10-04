package anytls

import "github.com/xtls/xray-core/features/stats"

const performanceStatsPrefix = "anytls>>>performance>>>"

type performanceStats struct {
	kernelTLSConnections stats.Counter
	kernelTLSActive      stats.Counter
	kernelTLSFallback    stats.Counter
	spliceConnections    stats.Counter
	spliceBytes          stats.Counter
	spliceErrors         stats.Counter
	writevBatches        stats.Counter
	writevBytes          stats.Counter
	writevSyscalls       stats.Counter
	writevErrors         stats.Counter
}

func newPerformanceStats(manager stats.Manager) *performanceStats {
	return &performanceStats{
		kernelTLSConnections: getOrRegisterPerformanceCounter(manager, performanceStatsPrefix+"ktls_connections"),
		kernelTLSActive:      getOrRegisterPerformanceCounter(manager, performanceStatsPrefix+"ktls_active_connections"),
		kernelTLSFallback:    getOrRegisterPerformanceCounter(manager, performanceStatsPrefix+"ktls_fallback_connections"),
		spliceConnections:    getOrRegisterPerformanceCounter(manager, performanceStatsPrefix+"splice_connections"),
		spliceBytes:          getOrRegisterPerformanceCounter(manager, performanceStatsPrefix+"splice_bytes"),
		spliceErrors:         getOrRegisterPerformanceCounter(manager, performanceStatsPrefix+"splice_errors"),
		writevBatches:        getOrRegisterPerformanceCounter(manager, performanceStatsPrefix+"writev_batches"),
		writevBytes:          getOrRegisterPerformanceCounter(manager, performanceStatsPrefix+"writev_bytes"),
		writevSyscalls:       getOrRegisterPerformanceCounter(manager, performanceStatsPrefix+"writev_syscalls"),
		writevErrors:         getOrRegisterPerformanceCounter(manager, performanceStatsPrefix+"writev_errors"),
	}
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

func (s *performanceStats) recordKernelTLS(active bool) {
	if s == nil {
		return
	}
	if s.kernelTLSConnections != nil {
		s.kernelTLSConnections.Add(1)
	}
	if active {
		if s.kernelTLSActive != nil {
			s.kernelTLSActive.Add(1)
		}
		return
	}
	if s.kernelTLSFallback != nil {
		s.kernelTLSFallback.Add(1)
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
