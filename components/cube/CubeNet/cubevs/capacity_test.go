package cubevs

import (
	"testing"

	"github.com/cilium/ebpf"
)

func TestResizePinnedMapRetainsEntriesAndIsIdempotent(t *testing.T) {
	mountBpffs(t)
	for _, kind := range []ebpf.MapType{ebpf.Hash, ebpf.LRUHash} {
		spec := &ebpf.MapSpec{Name: "capacity_test", Type: kind, KeySize: 4, ValueSize: 8, MaxEntries: 32}
		old, err := ebpf.NewMap(spec)
		if err != nil {
			t.Fatal(err)
		}
		defer old.Close()
		name := "capacity_test"
		if err := old.Pin(pinPath(name)); err != nil {
			t.Fatal(err)
		}
		for k := uint32(0); k < 4; k++ {
			if err := old.Put(k, uint64(k+42)); err != nil {
				t.Fatal(err)
			}
		}
		want := spec.Copy()
		want.MaxEntries = 8
		if err := resizePinnedMap(name, want); err != nil {
			t.Fatal(err)
		}
		current, err := ebpf.LoadPinnedMap(pinPath(name), nil)
		if err != nil {
			t.Fatal(err)
		}
		info, err := current.Info()
		if err != nil || info.MaxEntries != 8 {
			t.Fatalf("new capacity: %v, %v", info, err)
		}
		for k := uint32(0); k < 4; k++ {
			var value uint64
			if err := current.Lookup(k, &value); err != nil || value != uint64(k+42) {
				t.Fatalf("entry lost: %d, %v", k, err)
			}
		}
		if err := resizePinnedMap(name, want); err != nil {
			t.Fatal(err)
		}
		current.Close()
		old.Unpin()
	}
}

func TestResizePinnedMapRejectsOverflowAndLayoutChange(t *testing.T) {
	mountBpffs(t)
	spec := &ebpf.MapSpec{Name: "capacity_test", Type: ebpf.LRUHash, KeySize: 4, ValueSize: 8, MaxEntries: 16}
	old, err := ebpf.NewMap(spec)
	if err != nil {
		t.Fatal(err)
	}
	defer old.Close()
	if err := old.Pin(pinPath("capacity_test")); err != nil {
		t.Fatal(err)
	}
	for k := uint32(0); k < 8; k++ {
		if err := old.Put(k, uint64(k)); err != nil {
			t.Fatal(err)
		}
	}
	for _, invalid := range []ebpf.MapSpec{
		{Name: spec.Name, Type: spec.Type, KeySize: 4, ValueSize: 8, MaxEntries: 4},
		{Name: spec.Name, Type: spec.Type, KeySize: 4, ValueSize: 4, MaxEntries: 16},
	} {
		if err := resizePinnedMap("capacity_test", &invalid); err == nil {
			t.Fatal("unsafe replacement accepted")
		}
		pinned, err := ebpf.LoadPinnedMap(pinPath("capacity_test"), nil)
		if err != nil {
			t.Fatal(err)
		}
		before, _ := old.Info()
		after, _ := pinned.Info()
		a, _ := before.ID()
		b, _ := after.ID()
		pinned.Close()
		if a != b {
			t.Fatal("failed resize replaced the original pin")
		}
	}
}

func TestShippedMapBudget(t *testing.T) {
	primary, err := loadMvmtap()
	if err != nil {
		t.Fatal(err)
	}
	for name, want := range map[string]uint32{"egress_sessions": 1048576, "ingress_sessions": 1048576, "local_port_mapping": 4096, "remote_port_mapping": 4096, "dns_query_track": 2048, "primary_delivery": 4096, "direct_neigh": 1024} {
		if got := primary.Maps[name].MaxEntries; got != want {
			t.Fatalf("%s capacity %d, want %d", name, got, want)
		}
	}
	for _, name := range []string{MapNameEgressSessions, MapNameIngressSessions} {
		if primary.Maps[name].Flags != 1 {
			t.Fatalf("%s must allocate entries on demand", name)
		}
	}
	lan, err := loadLan()
	if err != nil {
		t.Fatal(err)
	}
	if lan.Maps[lanEndpointMap].MaxEntries != 512 || lan.Maps[lanFlowMap].MaxEntries != 8192 {
		t.Fatal("oversized LAN maps")
	}
}
