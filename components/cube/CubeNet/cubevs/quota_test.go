package cubevs

import (
	"encoding/binary"
	"encoding/json"
	"fmt"
	"os"
	"strconv"
	"strings"
	"testing"
	"time"

	"github.com/cilium/ebpf"
)

func TestQuotaCapacityKeepsGuestAndHostReserve(t *testing.T) {
	c, err := QuotaCapacityForMemory(958*1024*1024, 256*1024*1024)
	if err != nil {
		t.Fatal(err)
	}
	if c.Total != 131072 || uint64(c.Total)*uint64(c.PerState)+c.Fixed > c.Budget {
		t.Fatalf("unsafe capacity: %+v", c)
	}
	if _, err := QuotaCapacityForMemory(512*1024*1024, 512*1024*1024); err == nil {
		t.Fatal("Guest budget oversold")
	}
}

func quotaTestObject(t *testing.T, capacity uint32) *ebpf.Collection {
	t.Helper()
	if os.Getenv("CUBE_RUN_BPF_TESTS") != "1" {
		t.Skip("requires explicitly owned isolated BPF host")
	}
	s, err := loadQuota()
	if err != nil {
		t.Fatal(err)
	}
	for _, m := range s.Maps {
		m.Pinning = ebpf.PinNone
	}
	s.Maps["cube_q_flow_v1"].MaxEntries = capacity
	obj, err := ebpf.NewCollection(s)
	if err != nil {
		t.Fatalf("quota verifier: %+v", err)
	}
	t.Cleanup(obj.Close)
	return obj
}
func quotaPut(t *testing.T, obj *ebpf.Collection, id uint64, maximum, burst uint32) {
	t.Helper()
	if err := obj.Maps["cube_q_cfg_v1"].Put(id, quotaQ_config{Maximum: maximum, Rate: 1, Burst: burst, Revision: 1}); err != nil {
		t.Fatal(err)
	}
	if err := obj.Maps["cube_q_count_v1"].Put(id, quotaQ_counter{Tokens: uint64(burst) * 1000000000}); err != nil {
		t.Fatal(err)
	}
}
func quotaPacket(port uint16, incoming bool) []byte {
	p := make([]byte, 14+20+20)
	binary.BigEndian.PutUint16(p[12:14], 0x0800)
	p[14] = 0x45
	p[23] = 6
	binary.BigEndian.PutUint16(p[16:18], 40)
	a, b := []byte{10, 0, 0, 2}, []byte{203, 0, 113, 1}
	if incoming {
		a, b = b, a
	}
	copy(p[26:30], a)
	copy(p[30:34], b)
	if incoming {
		binary.BigEndian.PutUint16(p[34:36], 443)
		binary.BigEndian.PutUint16(p[36:38], port)
		p[47] = 0x12
	} else {
		binary.BigEndian.PutUint16(p[34:36], port)
		binary.BigEndian.PutUint16(p[36:38], 443)
		p[47] = 2
	}
	p[46] = 0x50
	return p
}
func quotaRun(t *testing.T, obj *ebpf.Collection, port uint16, incoming bool) uint32 {
	t.Helper()
	name := "cube_q_in"
	if incoming {
		name = "cube_q_out"
	}
	ctx := make([]byte, 80)
	binary.LittleEndian.PutUint32(ctx[36:40], 1)
	binary.LittleEndian.PutUint32(ctx[40:44], 1)
	ret, err := obj.Programs[name].Run(&ebpf.RunOptions{Data: quotaPacket(port, incoming), Context: ctx})
	if err != nil {
		t.Fatal(err)
	}
	return ret
}
func quotaCounter(t *testing.T, obj *ebpf.Collection, id uint64) quotaQ_counter {
	t.Helper()
	var c quotaQ_counter
	if err := obj.Maps["cube_q_count_v1"].Lookup(id, &c); err != nil {
		t.Fatal(err)
	}
	return c
}
func TestQuotaBPFSharedLimitReverseFlowShrinkAndRecovery(t *testing.T) {
	obj := quotaTestObject(t, 16)
	quotaPut(t, obj, 0, 16, 100)
	quotaPut(t, obj, 1, 2, 100)
	quotaPut(t, obj, 2, 2, 100)
	ep := quotaQ_endpoint{Owner: 1, Token: 100}
	if err := obj.Maps["cube_q_ep_v1"].Put(uint32(1), ep); err != nil {
		t.Fatal(err)
	}
	for _, port := range []uint16{10001, 10002} {
		if got := quotaRun(t, obj, port, false); got != ^uint32(0) {
			t.Fatalf("admission must continue policy, got %d", got)
		}
	}
	if got := quotaRun(t, obj, 10003, false); got != 2 {
		t.Fatalf("over quota admitted: %d", got)
	}
	if got := quotaRun(t, obj, 10001, true); got != ^uint32(0) {
		t.Fatalf("reverse flow charged twice: %d", got)
	}
	if c := quotaCounter(t, obj, 1); c.Count != 2 {
		t.Fatalf("shared accounting: %+v", c)
	}
	ep.Token = 101
	if err := obj.Maps["cube_q_ep_v1"].Put(uint32(1), ep); err != nil {
		t.Fatal(err)
	}
	if got := quotaRun(t, obj, 10004, false); got != 2 {
		t.Fatal("another NIC bypassed shared owner limit")
	}
	ep.Owner = 2
	if err := obj.Maps["cube_q_ep_v1"].Put(uint32(1), ep); err != nil {
		t.Fatal(err)
	}
	if got := quotaRun(t, obj, 10004, false); got != ^uint32(0) {
		t.Fatal("one user exhausted another user's quota")
	}
	ep.Owner = 1
	ep.Token = 100
	_ = obj.Maps["cube_q_ep_v1"].Put(uint32(1), ep)
	_ = obj.Maps["cube_q_cfg_v1"].Put(uint64(1), quotaQ_config{Maximum: 1, Rate: 1, Burst: 100, Revision: 2})
	if got := quotaRun(t, obj, 10001, false); got != ^uint32(0) {
		t.Fatal("shrinking quota dropped existing state")
	}
	if got := quotaRun(t, obj, 10005, false); got != 2 {
		t.Fatal("shrinking quota admitted a new flow")
	}
	var k quotaQ_key
	var v quotaQ_flow
	it := obj.Maps["cube_q_flow_v1"].Iterate()
	keys := []quotaQ_key{}
	for it.Next(&k, &v) {
		keys = append(keys, k)
	}
	if err := it.Err(); err != nil {
		t.Fatal(err)
	}
	// Updating an existing Hash value may replace its node; snapshot keys
	// before mutating rather than invalidating the iterator's current key.
	for _, key := range keys {
		if err := obj.Maps["cube_q_flow_v1"].Lookup(key, &v); err != nil {
			t.Fatal(err)
		}
		v.Expires = 0
		if err := obj.Maps["cube_q_flow_v1"].Put(key, v); err != nil {
			t.Fatal(err)
		}
	}
	if _, _, err := obj.Programs["cube_q_sweep"].Test(make([]byte, 64)); err != nil {
		t.Fatal(err)
	}
	if c := quotaCounter(t, obj, 0); c.Count != 0 {
		t.Fatalf("expired state leaked capacity: %+v", c)
	}
	if got := quotaRun(t, obj, 10005, false); got != ^uint32(0) {
		t.Fatal("expired capacity did not become reusable")
	}
}
func TestQuotaBPFRateAndAllocationFailureDoNotLeakSlots(t *testing.T) {
	obj := quotaTestObject(t, 1)
	quotaPut(t, obj, 0, 20, 100)
	quotaPut(t, obj, 1, 10, 1)
	if err := obj.Maps["cube_q_ep_v1"].Put(uint32(1), quotaQ_endpoint{Owner: 1, Token: 99}); err != nil {
		t.Fatal(err)
	}
	if quotaRun(t, obj, 20001, false) != ^uint32(0) || quotaRun(t, obj, 20002, false) != 2 {
		t.Fatal("token bucket did not limit burst")
	}
	if c := quotaCounter(t, obj, 1); c.Count != 1 || c.DeniedRate != 1 {
		t.Fatalf("rate counter: %+v", c)
	}
	// Refill only in this isolated test; production never replaces live counters.
	c := quotaCounter(t, obj, 1)
	c.Tokens = 10 * 1000000000
	_ = obj.Maps["cube_q_count_v1"].Put(uint64(1), c)
	if quotaRun(t, obj, 20003, false) != 2 {
		t.Fatal("full map accepted a new flow")
	}
	if c := quotaCounter(t, obj, 1); c.Count != 1 || c.AllocationFailures != 1 {
		t.Fatalf("allocation failure leaked a slot: %+v", c)
	}
	if c := quotaCounter(t, obj, 0); c.Count != 1 {
		t.Fatal("host counter leaked capacity")
	}
}

func TestQuotaBPFRetiredEndpointReleasesOnlyItsStates(t *testing.T) {
	obj := quotaTestObject(t, 16)
	quotaPut(t, obj, 0, 16, 100)
	quotaPut(t, obj, 1, 10, 100)
	ep := quotaQ_endpoint{Owner: 1, Token: 100}
	if err := obj.Maps["cube_q_ep_v1"].Put(uint32(1), ep); err != nil {
		t.Fatal(err)
	}
	if quotaRun(t, obj, 10001, false) != ^uint32(0) {
		t.Fatal("first admission")
	}
	ep.Token = 101
	if err := obj.Maps["cube_q_ep_v1"].Put(uint32(1), ep); err != nil {
		t.Fatal(err)
	}
	if quotaRun(t, obj, 10002, false) != ^uint32(0) {
		t.Fatal("second admission")
	}
	if err := obj.Maps["cube_q_retire_v1"].Put(uint64(100), uint8(1)); err != nil {
		t.Fatal(err)
	}
	if _, _, err := obj.Programs["cube_q_sweep"].Test(make([]byte, 64)); err != nil {
		t.Fatal(err)
	}
	if c := quotaCounter(t, obj, 1); c.Count != 1 {
		t.Fatalf("retirement affected another endpoint or leaked: %+v", c)
	}
	if c := quotaCounter(t, obj, 0); c.Count != 1 {
		t.Fatalf("host retirement leaked: %+v", c)
	}
	if quotaRun(t, obj, 10002, true) != ^uint32(0) {
		t.Fatal("surviving reverse flow")
	}
}

// Explicitly owned 1 GiB hosts only: measure actual lazy allocation at several
// occupancies, with both real NAT map layouts and the quota flow ledger.
func TestQuotaCapacityKernelCalibration(t *testing.T) {
	if os.Getenv("CUBE_QUOTA_CALIBRATE") != "1" {
		t.Skip("requires isolated capacity calibration host")
	}
	obj := quotaTestObject(t, 262144)
	spec, err := loadMvmtap()
	if err != nil {
		t.Fatal(err)
	}
	maps := []*ebpf.Map{}
	for _, name := range []string{"egress_sessions", "ingress_sessions"} {
		s := spec.Maps[name].Copy()
		s.MaxEntries = 262144
		s.Pinning = ebpf.PinNone
		m, err := ebpf.NewMap(s)
		if err != nil {
			t.Fatal(err)
		}
		defer m.Close()
		maps = append(maps, m)
	}
	slab := func() uint64 {
		raw, err := os.ReadFile("/proc/meminfo")
		if err != nil {
			t.Fatal(err)
		}
		for _, line := range strings.Split(string(raw), "\n") {
			f := strings.Fields(line)
			if len(f) > 1 && f[0] == "Slab:" {
				n, _ := strconv.ParseUint(f[1], 10, 64)
				return n * 1024
			}
		}
		t.Fatal("Slab unavailable")
		return 0
	}
	nf := uint64(0)
	raw, err := os.ReadFile("/proc/slabinfo")
	if err != nil {
		t.Fatal(err)
	}
	for _, line := range strings.Split(string(raw), "\n") {
		f := strings.Fields(line)
		if len(f) > 3 && f[0] == "nf_conntrack" {
			nf, _ = strconv.ParseUint(f[3], 10, 64)
		}
	}
	if nf == 0 {
		// Minimal test kernels omit netfilter. Use a separately recorded
		// production slab object size, never silently invent a measurement.
		nf, _ = strconv.ParseUint(os.Getenv("CUBE_QUOTA_CONNTRACK_OBJECT_BYTES"), 10, 64)
		if nf == 0 {
			t.Fatal("conntrack slab unavailable; explicit measured production object size required")
		}
		fmt.Printf("conntrack_source=explicit-production-slab object_bytes=%d\n", nf)
	}
	time.Sleep(time.Second)
	base := slab()
	previous := uint32(0)
	for _, n := range []uint32{16384, 65536, 131072} {
		for i := previous; i < n; i++ {
			key := quotaQ_key{Token: uint64(i) + 1}
			v := quotaQ_flow{Owner: 1}
			if err := obj.Maps["cube_q_flow_v1"].Put(key, v); err != nil {
				t.Fatal(err)
			}
			for j, m := range maps {
				s := spec.Maps[[]string{"egress_sessions", "ingress_sessions"}[j]]
				k := make([]byte, s.KeySize)
				v := make([]byte, s.ValueSize)
				binary.LittleEndian.PutUint32(k, i+1)
				if err := m.Put(k, v); err != nil {
					t.Fatal(err)
				}
			}
		}
		time.Sleep(time.Second)
		current := slab()
		delta := uint64(0)
		if current > base {
			delta = current - base
		}
		per := delta / uint64(n)
		row, _ := json.Marshal(map[string]any{"states": n, "slab_delta_bytes": delta, "bpf_bytes_per_state": per, "conntrack_object_bytes": nf, "two_conntrack_with_50_percent_overhead": nf * 3, "modeled_bytes_per_state": 1536})
		fmt.Println(string(row))
		if per+nf*3 > 1536 {
			t.Fatalf("network state model understates measured allocation: BPF %d + conntrack allowance %d", per, nf*3)
		}
		previous = n
	}
}
