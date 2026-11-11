// Copyright (c) 2026 Brocade contributors.
// SPDX-License-Identifier: Apache-2.0
package attachments

import (
	"context"
	"encoding/json"
	"os"
	"testing"

	wire "github.com/tencentcloud/CubeSandbox/pkgs/proto/networkattachments"
)

func managerConfig(t *testing.T) Config {
	t.Helper()
	root := t.TempDir()
	if err := os.Chmod(root, 0700); err != nil {
		t.Fatal(err)
	}
	return Config{StateDir: root, VMMRoot: "/run/vc/vm", MaxInterfaces: 3}
}
func managerRequest(id string) wire.Request {
	routing := json.RawMessage(`{"revision":1,"routes":[],"priorities":[],"dns":[]}`)
	return wire.Request{Routing: routing, Contract: wire.Contract, InstanceID: id, RuntimeID: id, Generation: 1, Interfaces: []wire.Interface{{ID: "nic-primary", SourceID: "public", Purpose: "contact"}, {ID: "nic-private", SourceID: "private", Purpose: "business", Allocated: &wire.AllocatedNIC{AllocationID: "allocation-test", PublicationID: "private", ResourceDomain: "test-domain", MAC: "02:01:02:03:04:05", IPv4: "10.120.0.2", IPv6: "fd12:3456::2", MTU: 1400}, DefaultIPv4: true, DefaultIPv6: true}}}
}
func TestReservationsAndStoppedLeasesSurviveManagerRestart(t *testing.T) {
	cfg := managerConfig(t)
	m, err := New(cfg, nil)
	if err != nil {
		t.Fatal(err)
	}
	req := managerRequest("vm-reservation")
	if _, err := m.Prepare(context.Background(), req); err != nil {
		t.Fatal(err)
	}
	before, _ := json.Marshal(m.server.records[req.InstanceID].NICs)
	req.Generation++
	req.Release = true
	if evidence, err := m.Reconcile(context.Background(), req); err != nil || !evidence.NetworkAbsent {
		t.Fatalf("empty physical release failed: %+v %v", evidence, err)
	}
	m, err = New(cfg, nil)
	if err != nil {
		t.Fatal(err)
	}
	req.Generation++
	req.Release = false
	req.RuntimeID = "new-runtime"
	if _, err := m.Prepare(context.Background(), req); err != nil {
		t.Fatal(err)
	}
	after, _ := json.Marshal(m.server.records[req.InstanceID].NICs)
	if string(before) != string(after) {
		t.Fatal("stopped recovery changed a stable device or address lease")
	}
	changed := req
	changed.Interfaces = append([]wire.Interface(nil), req.Interfaces...)
	changed.Interfaces[1].DefaultIPv6 = false
	if _, err := m.Prepare(context.Background(), changed); err == nil {
		t.Fatal("same-generation mutation was accepted")
	}
}
func TestInvalidDesiredListDoesNotAllocateARecord(t *testing.T) {
	m, err := New(managerConfig(t), nil)
	if err != nil {
		t.Fatal(err)
	}
	req := managerRequest("vm-invalid")
	req.Interfaces[1].Allocated = nil
	if _, err := m.Reconcile(context.Background(), req); err == nil {
		t.Fatal("unadvertised source was accepted")
	}
	if len(m.server.records) != 0 {
		t.Fatal("invalid request allocated execution state")
	}
}

func TestStoppedDetachProvesLeaseRemovalAndSurvivesReplay(t *testing.T) {
	cfg := managerConfig(t)
	m, err := New(cfg, nil)
	if err != nil {
		t.Fatal(err)
	}
	req := managerRequest("vm-stopped-detach")
	if _, err := m.Prepare(context.Background(), req); err != nil {
		t.Fatal(err)
	}
	req.Generation++
	req.Release = true
	if _, err := m.Reconcile(context.Background(), req); err != nil {
		t.Fatal(err)
	}
	req.Generation++
	req.Interfaces = req.Interfaces[:1]
	req.Detach = []string{"nic-private"}
	for attempt := 0; attempt < 2; attempt++ {
		result, err := m.Reconcile(context.Background(), req)
		if err != nil || !result.NetworkAbsent || len(result.Detached) != 1 || result.Detached[0] != "nic-private" {
			t.Fatalf("missing stopped detach proof: %+v %v", result, err)
		}
		nic := m.server.records[req.InstanceID].NICs["nic-private"]
		if nic != nil && (!nic.Removed || nic.Ifindex != 0 || nic.RemovedGeneration != req.Generation) {
			t.Fatal("cleanup receipt preceded durable lease removal")
		}
		if _, err := m.Prepare(context.Background(), req); err != nil {
			t.Fatal(err)
		}
		m, err = New(cfg, nil)
		if err != nil {
			t.Fatal(err)
		}
	}
	changed := req
	changed.Detach = []string{"another-nic"}
	if _, err := m.Reconcile(context.Background(), changed); err == nil {
		t.Fatal("same-generation cleanup assignment changed after restart")
	}
	req.Generation++
	req.Release = false
	req.Detach = nil
	if _, err := m.Prepare(context.Background(), req); err != nil {
		t.Fatal(err)
	}
	if nic := m.server.records[req.InstanceID].NICs["nic-private"]; nic != nil && !nic.Removed {
		t.Fatal("boot resurrected a removed business lease")
	}
}

func TestInterruptedReleaseRemainsDurableAndBlocksReallocation(t *testing.T) {
	cfg := managerConfig(t)
	m, err := New(cfg, nil)
	if err != nil {
		t.Fatal(err)
	}
	req := managerRequest("vm-interrupted")
	if _, err := m.Prepare(context.Background(), req); err != nil {
		t.Fatal(err)
	}
	m.server.records[req.InstanceID].NICs["nic-private"].Ifindex = 123456
	if err := m.server.persist(m.server.records[req.InstanceID]); err != nil {
		t.Fatal(err)
	}
	req.Release = true
	req.Forget = true
	req.Generation++
	if _, err := m.Reconcile(context.Background(), req); err == nil {
		t.Fatal("incomplete kernel cleanup was acknowledged")
	}
	m, err = New(cfg, nil)
	if err != nil {
		t.Fatal(err)
	}
	if !m.server.records[req.InstanceID].Releasing {
		t.Fatal("release intent was lost on restart")
	}
	next := req
	next.Release = false
	next.Generation++
	if _, err := m.Prepare(context.Background(), next); err == nil {
		t.Fatal("reallocation passed an unfinished release")
	}
	m.server.records[req.InstanceID].NICs["nic-private"].Ifindex = 0
	if err := m.server.persist(m.server.records[req.InstanceID]); err != nil {
		t.Fatal(err)
	}
	if evidence, err := m.Reconcile(context.Background(), req); err != nil || !evidence.NetworkAbsent {
		t.Fatalf("retry failed: %+v %v", evidence, err)
	}
}

func TestDeletedAttachmentStateCannotBeRetainedOrReactivated(t *testing.T) {
	m, err := New(managerConfig(t), nil)
	if err != nil {
		t.Fatal(err)
	}
	req := managerRequest("vm-terminal")
	if _, err := m.Prepare(context.Background(), req); err != nil {
		t.Fatal(err)
	}
	req.Release = true
	req.Forget = true
	req.Generation++
	if _, err := m.Reconcile(context.Background(), req); err != nil {
		t.Fatal(err)
	}
	if evidence, err := m.Reconcile(context.Background(), req); err != nil || !evidence.NetworkAbsent {
		t.Fatal("idempotent deletion failed")
	}
	req.Forget = false
	if _, err := m.Reconcile(context.Background(), req); err == nil {
		t.Fatal("deletion was changed into retention")
	}
	req.Release = false
	req.Generation++
	if _, err := m.Prepare(context.Background(), req); err == nil {
		t.Fatal("deleted attachment identity was reactivated")
	}
}

func TestPersistedLeasesBlockReuseUntilConfirmedRelease(t *testing.T) {
	cfg := managerConfig(t)
	m, err := New(cfg, nil)
	if err != nil {
		t.Fatal(err)
	}
	first := managerRequest("vm-first")
	first.Interfaces[1].DefaultIPv6 = false
	if _, err := m.Prepare(context.Background(), first); err != nil {
		t.Fatal(err)
	}
	m, err = New(cfg, nil)
	if err != nil {
		t.Fatal(err)
	}
	next := managerRequest("vm-next")
	next.Interfaces[1].DefaultIPv6 = false
	if _, err := m.Prepare(context.Background(), next); err == nil {
		t.Fatal("persisted address was reused")
	}
	first.Release, first.Forget = true, true
	if _, err := m.Reconcile(context.Background(), first); err != nil {
		t.Fatal(err)
	}
	if _, err := m.Prepare(context.Background(), next); err != nil {
		t.Fatal(err)
	}
}

func assignedInterface(id, publication, address string) object {
	return object{"id": id, "source_id": publication, "purpose": "business", "allocated": object{"allocation_id": "allocation-" + id, "publication_id": publication, "resource_domain": "test-domain", "mac": "02:01:02:03:04:05", "ipv4": address, "mtu": 1400}}
}
