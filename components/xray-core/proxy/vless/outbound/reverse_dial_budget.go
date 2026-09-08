package outbound

import "sync"

// A node-wide admission budget. Each unavailable pair gets its first in-flight
// dial before healthy pairs consume slots for spares. No network work runs here.
var reverseDials = struct {
	sync.Mutex
	active int
	pairs  map[*Reverse]reverseDialDemand
}{pairs: make(map[*Reverse]reverseDialDemand)}

type reverseDialDemand struct {
	ready, pending int
	eligible       bool
}

func (r *Reverse) reportDialDemand(ready, pending int, eligible bool) {
	reverseDials.Lock()
	reverseDials.pairs[r] = reverseDialDemand{ready, pending, eligible}
	reverseDials.Unlock()
}
func (r *Reverse) acquireDial() bool {
	reverseDials.Lock()
	defer reverseDials.Unlock()
	if reverseDials.active >= 32 {
		return false
	}
	mine := reverseDials.pairs[r]
	if mine.ready+mine.pending > 0 {
		for pair, demand := range reverseDials.pairs {
			if pair != r && demand.eligible && demand.ready+demand.pending == 0 {
				return false
			}
		}
	}
	reverseDials.active++
	mine.pending++
	reverseDials.pairs[r] = mine
	return true
}
func releaseReverseDial() { reverseDials.Lock(); reverseDials.active--; reverseDials.Unlock() }
func (r *Reverse) forgetDialDemand() {
	reverseDials.Lock()
	delete(reverseDials.pairs, r)
	reverseDials.Unlock()
}
