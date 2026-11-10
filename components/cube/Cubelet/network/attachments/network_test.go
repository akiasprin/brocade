package attachments

import (
	"context"
	"encoding/json"
	"os"
	"testing"
)

func TestAttachmentIdentityAndAddressesSurviveRestart(t *testing.T) {
	s := testServer(t)
	r := &record{Instance: "cvm-test", Runtime: "cvm-test", ContactID: "nic-contact"}
	s.records[r.Instance] = r
	d := desired()
	spec := obj(d, "spec")
	spec["interfaces"] = append(array(spec, "interfaces"), assignedInterface("nic-biz", "internal", "10.120.0.2"))
	if e := s.prepareNICs(r, d); e != nil {
		t.Fatal(e)
	}
	before, _ := json.Marshal(r.NICs)
	manager, e := New(Config{StateDir: s.root, VMMRoot: "/run/vc/vm", MaxInterfaces: 3}, nil)
	if e != nil {
		t.Fatal(e)
	}
	s = manager.server
	r = s.records[r.Instance]
	if e := s.prepareNICs(r, d); e != nil {
		t.Fatal(e)
	}
	after, _ := json.Marshal(r.NICs)
	if string(before) != string(after) {
		t.Fatal("restart changed a device or address identity")
	}
	r.NICs["nic-biz"].Tap = "foreign-tap"
	if e := s.persist(r); e != nil {
		t.Fatal(e)
	}
	if _, e := New(Config{StateDir: s.root, VMMRoot: "/run/vc/vm", MaxInterfaces: 3}, nil); e == nil {
		t.Fatal("corrupt TAP binding was accepted")
	}
}

func TestPolicyConflictCannotBeMistakenForContactIsolation(t *testing.T) {
	if !(policyRule{"-4", 30050, "all", "169.254.68.4/30", contactTable, "", 0, 0}).matches(object{"priority": 30050, "src": "all", "dst": "169.254.68.4", "dstlen": 30, "table": "30000"}) {
		t.Fatal("actual iproute2 numeric-string table and separate prefix length rejected")
	}
	rules := []policyRule{{"-4", 30010, "169.254.68.6", "all", contactTable, "", 0, 0}, {"-4", businessPriority, "all", "all", businessTable, "", 0, 0}}
	actual := []object{{"priority": 30010, "src": "169.254.68.6", "table": 30000}, {"priority": 30100, "src": "all", "table": 30001}}
	if commands, e := inspectRules(actual, rules, "-4"); e != nil || len(commands) != 0 {
		t.Fatalf("idempotent rule rejected: %v %v", commands, e)
	}
	actual[1]["table"] = 254
	if _, e := inspectRules(actual, rules, "-4"); e == nil {
		t.Fatal("public fallback rule was accepted as business isolation")
	}
	actual[1]["table"] = 30001
	actual[1]["fwmark"] = 1
	if _, e := inspectRules(actual, rules, "-4"); e == nil {
		t.Fatal("a selective rule was accepted as a catch-all")
	}
}

func testServer(t *testing.T) *server {
	root := t.TempDir()
	if err := os.Chmod(root, 0700); err != nil {
		t.Fatal(err)
	}
	return &server{ctx: context.Background(), root: root, cfg: config{Capabilities: object{"max_interfaces": 1, "network_sources": []any{object{"id": "contact", "kind": "public"}}}}, records: map[string]*record{}}
}
func desired() object {
	return object{"id": "cvm-test", "generation": uint64(1), "spec": object{"interfaces": []any{object{"id": "nic-contact", "source_id": "contact", "purpose": "contact"}}}}
}
