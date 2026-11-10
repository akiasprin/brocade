package cubevs

import (
	"errors"
	"fmt"
	"os"
	"slices"

	"github.com/cilium/ebpf"
)

// Only these owned maps change capacity. Policy maps and their inner-map ABI
// stay untouched. Old attached programs retain their map FDs until replacement.
func resizePinnedMaps(spec *ebpf.CollectionSpec) error {
	names := []string{MapNameEgressSessions, MapNameIngressSessions,
		"local_port_mapping", "remote_port_mapping", lanEndpointMap, lanStatsMap, lanFlowMap}
	slices.Sort(names)
	for _, name := range names {
		wanted := spec.Maps[name]
		if wanted == nil {
			continue
		}
		if err := resizePinnedMap(name, wanted); err != nil {
			return fmt.Errorf("resize CubeVS map %s: %w", name, err)
		}
	}
	return nil
}

func resizePinnedMap(name string, wanted *ebpf.MapSpec) error {
	old, err := ebpf.LoadPinnedMap(pinPath(name), nil)
	if errors.Is(err, os.ErrNotExist) {
		return nil
	}
	if err != nil {
		return err
	}
	defer old.Close()
	info, err := old.Info()
	if err != nil {
		return err
	}
	forwardNATAllocation := (name == MapNameEgressSessions || name == MapNameIngressSessions) && info.Type == ebpf.Hash && info.Flags == 0 && wanted.Flags == 1
	if info.Type != wanted.Type || info.KeySize != wanted.KeySize || info.ValueSize != wanted.ValueSize || (info.Flags != wanted.Flags && !forwardNATAllocation) {
		return errors.New("existing map layout differs; original pin retained")
	}
	if info.MaxEntries == wanted.MaxEntries && info.Flags == wanted.Flags {
		return nil
	}
	if wanted.Type != ebpf.Hash && wanted.Type != ebpf.LRUHash {
		return errors.New("unsupported map capacity change; original pin retained")
	}
	// Count first: an LRU map would otherwise silently evict copied live entries.
	count := uint32(0)
	key, value := make([]byte, info.KeySize), make([]byte, info.ValueSize)
	iter := old.Iterate()
	for iter.Next(&key, &value) {
		count++
		if count > wanted.MaxEntries {
			return errors.New("live map entries exceed the new capacity; original pin retained")
		}
	}
	if err := iter.Err(); err != nil {
		return err
	}
	copySpec := wanted.Copy()
	copySpec.Pinning = ebpf.PinNone
	replacement, err := ebpf.NewMap(copySpec)
	if err != nil {
		return err
	}
	defer replacement.Close()
	iter = old.Iterate()
	count = 0
	for iter.Next(&key, &value) {
		count++
		if count > wanted.MaxEntries {
			return errors.New("map grew during resize; original pin retained")
		}
		if err := replacement.Update(key, value, ebpf.UpdateNoExist); err != nil {
			return fmt.Errorf("copy map entry: %w", err)
		}
	}
	if err := iter.Err(); err != nil {
		return err
	}
	// One owned candidate pin per map bounds interrupted-upgrade residue.
	// The public pin always remains complete until the atomic rename.
	candidate := pinPath(name + "_capacity")
	stale, err := ebpf.LoadPinnedMap(candidate, nil)
	if err == nil {
		staleInfo, inspectErr := stale.Info()
		stale.Close()
		if inspectErr != nil || staleInfo.Type != wanted.Type || staleInfo.KeySize != wanted.KeySize || staleInfo.ValueSize != wanted.ValueSize || staleInfo.Flags != wanted.Flags || staleInfo.MaxEntries != wanted.MaxEntries {
			return errors.New("resize candidate has an unexpected identity; pins retained")
		}
		if err := os.Remove(candidate); err != nil {
			return err
		}
	} else if !errors.Is(err, os.ErrNotExist) {
		return err
	}
	if err := replacement.Pin(candidate); err != nil {
		return err
	}
	defer os.Remove(candidate)
	return os.Rename(candidate, pinPath(name))
}
