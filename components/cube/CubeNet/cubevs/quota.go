// Copyright (c) 2026 Brocade contributors.
// SPDX-License-Identifier: Apache-2.0
package cubevs

import (
	"errors"
	"fmt"
	"net"
	"os"
	"strconv"
	"strings"
	"sync"
	"syscall"
	"time"

	"github.com/cilium/ebpf"
	"github.com/florianl/go-tc"
	"golang.org/x/sys/unix"
)

const QuotaContract = "cubevs-user-quota-v1"

type QuotaLimits struct {
	Maximum uint32 `json:"max_states"`
	Rate    uint32 `json:"new_per_second"`
	Burst   uint32 `json:"burst"`
}
type QuotaEndpoint struct {
	Index     uint32 `json:"ifindex"`
	Instance  string `json:"instance_id"`
	Interface string `json:"interface_id"`
}
type QuotaAssignment struct {
	ID        string      `json:"id"`
	Revision  uint64      `json:"revision"`
	Limits    QuotaLimits `json:"limits"`
	Instances []string    `json:"instances"`
}
type QuotaReport struct {
	ID                 string  `json:"id"`
	Revision           uint64  `json:"revision"`
	Applied            bool    `json:"applied"`
	States             uint64  `json:"states"`
	DeniedCapacity     uint64  `json:"denied_capacity"`
	DeniedRate         uint64  `json:"denied_rate"`
	AllocationFailures uint64  `json:"allocation_failures"`
	Error              *string `json:"error"`
}
type QuotaCapacity struct {
	Physical     uint64 `json:"physical_memory_bytes"`
	HostReserved uint64 `json:"host_reserved_memory_bytes"`
	Contract     string `json:"contract"`
	Total        uint32 `json:"total_states"`
	Reserved     uint32 `json:"system_reserved_states"`
	Budget       uint64 `json:"memory_budget_bytes"`
	Fixed        uint64 `json:"fixed_memory_bytes"`
	PerState     uint32 `json:"bytes_per_state"`
	Model        string `json:"model"`
}

// The budget includes both NAT indexes, the quota ledger and non-BPF network
// state. A model revision is observable; it is not a measured byte counter.
func QuotaCapacityForMemory(physical, guestBudget uint64) (QuotaCapacity, error) {
	const mib = uint64(1024 * 1024)
	c := QuotaCapacity{Contract: QuotaContract, Reserved: 4096, Fixed: 16 * mib,
		Physical: physical, HostReserved: max(448*mib, physical/16), PerState: 1536, Model: "network-state-budget-v1"}
	if physical <= guestBudget || physical-guestBudget <= c.HostReserved {
		return c, errors.New("host memory cannot cover Guest and network budgets")
	}
	c.Budget = physical - guestBudget - c.HostReserved
	if c.Budget <= c.Fixed {
		return c, errors.New("network fixed budget unavailable")
	}
	maximum := min((c.Budget-c.Fixed)/(uint64(c.PerState)+48), uint64(1048576))
	for n := uint32(8192); uint64(n) <= maximum; n *= 2 {
		c.Total = n
	}
	if c.Total == 0 {
		return c, errors.New("network minimum capacity unavailable")
	}
	c.Fixed += uint64(c.Total) * 48
	return c, nil
}

func PrepareQuota(capacity uint32) error {
	quotaState.Lock()
	defer quotaState.Unlock()
	_, err := loadQuotaRuntime(capacity)
	return err
}

var quotaState struct {
	sync.Mutex
	collection *ebpf.Collection
	capacity   uint32
	byInstance map[string]uint64
	retired    map[uint64]time.Time
}

func loadQuotaRuntime(capacity uint32) (*ebpf.Collection, error) {
	if quotaState.collection != nil {
		if capacity > quotaState.capacity {
			return nil, errors.New("quota map hard capacity requires a controlled runtime upgrade")
		}
		return quotaState.collection, nil
	}
	spec, err := loadQuota()
	if err != nil {
		return nil, err
	}
	spec.Maps["cube_q_flow_v1"].MaxEntries = capacity
	obj, err := ebpf.NewCollectionWithOptions(spec, ebpf.CollectionOptions{Maps: ebpf.MapOptions{PinPath: bpfFSPath}})
	if err != nil {
		return nil, fmt.Errorf("load quota kernel state: %w", err)
	}
	quotaState.collection, quotaState.capacity = obj, capacity
	return obj, nil
}

func quotaIdentity(id string) (uint64, error) {
	if len(id) != 16 {
		return 0, errors.New("invalid quota identity")
	}
	n, err := strconv.ParseUint(id, 16, 64)
	if err != nil || n == 0 {
		return 0, errors.New("invalid quota identity")
	}
	return n, nil
}

// ApplyQuota uses the trusted Cubelet Unix RPC. It never accepts a tenant's
// chosen host link: endpoints must have been resolved from Cubelet ownership.
func ApplyQuota(capacity QuotaCapacity, assignments []QuotaAssignment, endpoints []QuotaEndpoint) ([]QuotaReport, error) {
	quotaState.Lock()
	defer quotaState.Unlock()
	if len(assignments) > 512 || len(endpoints) > 8192 {
		return nil, errors.New("quota request exceeds host bound")
	}
	byInstance := map[string]uint64{}
	seen := map[uint64]bool{}
	total := uint64(0)
	for _, a := range assignments {
		id, err := quotaIdentity(a.ID)
		if err != nil {
			return nil, err
		}
		if seen[id] || a.Revision == 0 || a.Limits.Rate == 0 || a.Limits.Rate > 100000 || a.Limits.Burst == 0 || a.Limits.Burst > 200000 || len(a.Instances) > 256 {
			return nil, errors.New("invalid quota limits or duplicate group")
		}
		seen[id] = true
		total += uint64(a.Limits.Maximum)
		for _, instance := range a.Instances {
			if instance == "" || len(instance) > 150 || byInstance[instance] != 0 {
				return nil, errors.New("ambiguous quota instance ownership")
			}
			byInstance[instance] = id
		}
	}
	if total > uint64(capacity.Total-capacity.Reserved) {
		return nil, errors.New("assigned quotas exceed allocatable host capacity")
	}
	obj, err := loadQuotaRuntime(capacity.Total)
	if err != nil {
		return nil, err
	}
	config, counters := obj.Maps["cube_q_cfg_v1"], obj.Maps["cube_q_count_v1"]
	ensure := func(id uint64, cfg quotaQ_config) error {
		var old quotaQ_config
		err := config.Lookup(id, &old)
		if err != nil && !errors.Is(err, ebpf.ErrKeyNotExist) {
			return err
		}
		if err == nil && (old.Revision > cfg.Revision || (old.Revision == cfg.Revision && old != cfg)) {
			return errors.New("stale or conflicting quota revision")
		}
		initial := quotaQ_counter{Tokens: uint64(cfg.Burst) * 1000000000}
		if err := counters.Update(id, initial, ebpf.UpdateNoExist); err != nil && !errors.Is(err, syscall.EEXIST) {
			return err
		}
		return config.Update(id, cfg, ebpf.UpdateAny)
	}
	if err := ensure(0, quotaQ_config{Maximum: capacity.Total, Rate: 100000, Burst: 200000, Revision: 1}); err != nil {
		return nil, err
	}
	for _, a := range assignments {
		id, _ := quotaIdentity(a.ID)
		if err := ensure(id, quotaQ_config{Maximum: a.Limits.Maximum, Rate: a.Limits.Rate, Burst: a.Limits.Burst, Revision: a.Revision}); err != nil {
			return nil, err
		}
	}
	quotaState.byInstance = byInstance
	if err := retireQuotaEndpoints(obj, byInstance); err != nil {
		return nil, err
	}
	for _, ep := range endpoints {
		if err := ensureQuotaEndpoint(obj, ep, byInstance[ep.Instance]); err != nil {
			return nil, err
		}
	}
	if err := reapQuota(obj); err != nil {
		return nil, err
	}
	reports := make([]QuotaReport, 0, len(assignments))
	for _, a := range assignments {
		id, _ := quotaIdentity(a.ID)
		var c quotaQ_counter
		if err := counters.Lookup(id, &c); err != nil {
			return nil, err
		}
		reports = append(reports, QuotaReport{ID: a.ID, Revision: a.Revision, Applied: true, States: uint64(c.Count),
			DeniedCapacity: c.DeniedCapacity, DeniedRate: c.DeniedRate, AllocationFailures: c.AllocationFailures})
	}
	return reports, nil
}

func ensureQuotaEndpoint(obj *ebpf.Collection, ep QuotaEndpoint, owner uint64) error {
	if owner == 0 {
		return nil
	}
	if ep.Index == 0 || ep.Instance == "" || ep.Interface == "" {
		return errors.New("invalid quota TAP assignment")
	}
	link, err := net.InterfaceByIndex(int(ep.Index))
	if err != nil {
		return err
	}
	if _, err := os.Stat("/sys/class/net/" + link.Name + "/tun_flags"); err != nil {
		return errors.New("quota attachment is not a TAP")
	}
	generation := uint32(0)
	if ep.Interface == "contact" {
		m, err := ebpf.LoadPinnedMap(pinPath("ifindex_to_mvmmeta"), nil)
		if err != nil {
			return err
		}
		var meta mvmtapMvmMeta
		err = m.Lookup(ep.Index, &meta)
		m.Close()
		if err != nil {
			return err
		}
		if strings.TrimRight(string(meta.Uuid[:]), "\x00") != ep.Instance {
			return errors.New("primary quota TAP has a different sandbox owner")
		}
		generation = meta.Version
	}
	token := endpointToken(ep.Instance, fmt.Sprintf("%s:%d:%d", ep.Interface, ep.Index, generation))
	var old quotaQ_endpoint
	if err := obj.Maps["cube_q_ep_v1"].Lookup(ep.Index, &old); err == nil && old.Token != token {
		if err := obj.Maps["cube_q_retire_v1"].Put(old.Token, uint8(1)); err != nil {
			return err
		}
	} else if err != nil && !errors.Is(err, ebpf.ErrKeyNotExist) {
		return err
	}
	for _, d := range []TCDirection{TCIngress, TCEgress} {
		name := "cube_q_in"
		if d == TCEgress {
			name = "cube_q_out"
		}
		if err := quotaFilter(ep.Index, d, obj.Programs[name], name); err != nil {
			return err
		}
	}
	return obj.Maps["cube_q_ep_v1"].Update(ep.Index, quotaQ_endpoint{Owner: owner, Token: token, Instance: endpointToken(ep.Instance, "instance")}, ebpf.UpdateAny)
}

func retireQuotaEndpoints(obj *ebpf.Collection, instances map[string]uint64) error {
	live := map[uint64]bool{}
	for id := range instances {
		live[endpointToken(id, "instance")] = true
	}
	type removed struct {
		index uint32
		token uint64
	}
	stale := []removed{}
	var index uint32
	var ep quotaQ_endpoint
	it := obj.Maps["cube_q_ep_v1"].Iterate()
	for it.Next(&index, &ep) {
		if !live[ep.Instance] {
			stale = append(stale, removed{index, ep.Token})
		}
	}
	if err := it.Err(); err != nil {
		return err
	}
	for _, r := range stale {
		if err := obj.Maps["cube_q_retire_v1"].Put(r.token, uint8(1)); err != nil {
			return err
		}
		if err := obj.Maps["cube_q_ep_v1"].Delete(r.index); err != nil && !errors.Is(err, ebpf.ErrKeyNotExist) {
			return err
		}
	}
	return nil
}

// Called before newly owned TAPs are exposed to a Guest, not on a later poll.
func EnsureQuotaEndpoint(instance, iface string, index uint32) error {
	quotaState.Lock()
	defer quotaState.Unlock()
	if quotaState.collection == nil || quotaState.byInstance == nil {
		return nil
	}
	if quotaState.byInstance[instance] == 0 {
		return errors.New("sandbox has no confirmed user-host quota assignment")
	}
	return ensureQuotaEndpoint(quotaState.collection, QuotaEndpoint{Index: index, Instance: instance, Interface: iface}, quotaState.byInstance[instance])
}

func quotaFilter(index uint32, direction TCDirection, p *ebpf.Program, name string) error {
	if err := createQdisc(index); err != nil {
		return err
	}
	conn, err := tc.Open(&tc.Config{})
	if err != nil {
		return err
	}
	defer conn.Close()
	parent := tcMakeHandle(tcHandleClsact, uint32(direction))
	const handle = 0x4251
	const priority = 5
	filters, err := conn.Filter().Get(&tc.Msg{Family: unix.AF_UNSPEC, Ifindex: index, Parent: parent})
	if err != nil {
		return err
	}
	for _, f := range filters {
		if f.Info>>16 < priority {
			if f.Info>>16 != 1 || f.Handle != tcFilterHandle || f.Kind != "bpf" || f.BPF == nil || f.BPF.Name == nil || (*f.BPF.Name != programNameFromCube && *f.BPF.Name != programNameFromWorld && *f.BPF.Name != programNameFromEnvoy) {
				return errors.New("an earlier unowned TC filter prevents verified quota enforcement")
			}
			program, err := ebpf.LoadPinnedProgram(pinPath(*f.BPF.Name), nil)
			if err != nil {
				return err
			}
			err = attachFilter(index, uint32(program.FD()), *f.BPF.Name, direction)
			program.Close()
			if err != nil {
				return err
			}
		}
		if f.Handle == handle && f.Info>>16 == priority {
			if f.Kind != "bpf" || f.BPF == nil || f.BPF.Name == nil || *f.BPF.Name != name {
				return errors.New("quota TC filter ownership conflicts")
			}
		}
	}
	fd, flags := uint32(p.FD()), uint32(tcFlagDirectAction)
	return conn.Filter().Replace(&tc.Object{Msg: tc.Msg{Family: unix.AF_UNSPEC, Ifindex: index, Handle: handle, Parent: parent,
		Info: tcMakeHandle(priority<<16, uint32(htons(syscall.ETH_P_ALL)))}, Attribute: tc.Attribute{Kind: "bpf", BPF: &tc.Bpf{FD: &fd, Name: &name, Flags: &flags}}})
}

func reapQuota(obj *ebpf.Collection) error {
	_, _, err := obj.Programs["cube_q_sweep"].Test(make([]byte, 64))
	if err != nil {
		return err
	}
	retired := []uint64{}
	var token uint64
	var flag uint8
	it := obj.Maps["cube_q_retire_v1"].Iterate()
	for it.Next(&token, &flag) {
		retired = append(retired, token)
	}
	if err := it.Err(); err != nil {
		return err
	}
	if quotaState.retired == nil {
		quotaState.retired = map[uint64]time.Time{}
	}
	now := time.Now()
	for _, token := range retired {
		since, ok := quotaState.retired[token]
		if !ok {
			quotaState.retired[token] = now
			continue
		}
		if now.Sub(since) < time.Minute {
			continue
		}
		if err := obj.Maps["cube_q_retire_v1"].Delete(token); err != nil {
			return err
		}
		delete(quotaState.retired, token)
	}
	return nil
}

func EffectiveQuotaMemory() (uint64, error) {
	var info unix.Sysinfo_t
	if err := unix.Sysinfo(&info); err != nil {
		return 0, err
	}
	physical := info.Totalram * uint64(info.Unit)
	// cgroup-v2 limits on the daemon and all ancestors also constrain kernel allocations.
	raw, err := os.ReadFile("/proc/self/cgroup")
	if err != nil {
		return 0, err
	}
	for _, line := range strings.Split(string(raw), "\n") {
		if strings.HasPrefix(line, "0::") {
			path := strings.TrimPrefix(line, "0::")
			for {
				data, e := os.ReadFile("/sys/fs/cgroup" + path + "/memory.max")
				if e == nil && strings.TrimSpace(string(data)) != "max" {
					v, e := strconv.ParseUint(strings.TrimSpace(string(data)), 10, 64)
					if e != nil {
						return 0, e
					}
					physical = min(physical, v)
				}
				if path == "" || path == "/" {
					break
				}
				at := strings.LastIndex(path, "/")
				path = path[:max(at, 0)]
			}
		}
	}
	return physical, nil
}
