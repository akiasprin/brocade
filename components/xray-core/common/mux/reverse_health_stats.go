package mux

import (
	"strconv"
	"sync/atomic"
)

// Fixed labels only: never worker IDs, account IDs, addresses or probe nonces.
// Counters survive worker replacement and are never reset by telemetry readers.
type reverseHealthCounters struct {
	created, closed, probes, acks, timeouts, rejected, queueFailures, telemetryDropped atomic.Int64
	rtt                                                                                [9]atomic.Int64
}

var reverseCounters [2]reverseHealthCounters
var reverseRTTBuckets = [...]int64{50, 100, 250, 500, 750, 1000, 2000, 5000, 10000}

func reverseRole(role string) int {
	if role == "bridge" {
		return 1
	}
	return 0
}
func GetReverseHealthMetrics() map[string]int64 {
	out := make(map[string]int64)
	for i, role := range []string{"portal", "bridge"} {
		c := &reverseCounters[i]
		prefix := "reverse>>>" + role + ">>>"
		out[prefix+"workers_created"] = c.created.Load()
		out[prefix+"workers_closed"] = c.closed.Load()
		out[prefix+"probe_sent"] = c.probes.Load()
		out[prefix+"probe_ack"] = c.acks.Load()
		out[prefix+"probe_timeout"] = c.timeouts.Load()
		out[prefix+"dispatch_rejected"] = c.rejected.Load()
		out[prefix+"control_queue_failures"] = c.queueFailures.Load()
		out[prefix+"telemetry_dropped"] = c.telemetryDropped.Load()
		for j, bucket := range reverseRTTBuckets {
			out[prefix+"rtt_ms_le_"+strconv.FormatInt(bucket, 10)] = c.rtt[j].Load()
		}
		for _, state := range []string{"VALIDATING", "READY", "SUSPECT", "DRAINING", "DEAD", "CLOSED"} {
			out[prefix+"workers>>>"+state] = 0
		}
	}
	for _, w := range GetReverseHealthSnapshot().Workers {
		out["reverse>>>"+w.Role+">>>workers>>>"+w.State]++
	}
	return out
}
