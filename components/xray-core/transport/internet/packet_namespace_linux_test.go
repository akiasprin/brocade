//go:build linux

package internet

import (
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestPacketNamespacePolicyRejectsUnsafeOrAmbiguousInput(t *testing.T) {
	boot, err := os.ReadFile("/proc/sys/kernel/random/boot_id")
	if err != nil {
		t.Fatal(err)
	}
	dir := t.TempDir()
	if err := os.Chmod(dir, 0700); err != nil {
		t.Fatal(err)
	}
	path := filepath.Join(dir, "policy.json")
	policy := packetNamespacePolicy{Version: 1, NamespacePath: "/run/netns/brocade-hy2-fixture", NamespaceIno: 1, NamespaceDev: 1, BootID: strings.TrimSpace(string(boot)), Listeners: []string{"[::]:30000"}}
	write := func(value any, mode os.FileMode) {
		t.Helper()
		data, err := json.Marshal(value)
		if err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(path, data, 0600); err != nil {
			t.Fatal(err)
		}
		if err := os.Chmod(path, mode); err != nil {
			t.Fatal(err)
		}
	}
	write(policy, 0600)
	if _, err := readPacketNamespacePolicy(path); err != nil {
		t.Fatal(err)
	}
	tests := []struct {
		name   string
		change func(*packetNamespacePolicy)
	}{
		{"version", func(p *packetNamespacePolicy) { p.Version = 2 }},
		{"boot", func(p *packetNamespacePolicy) { p.BootID = "another-boot" }},
		{"path", func(p *packetNamespacePolicy) { p.NamespacePath = "/proc/1/ns/net" }},
		{"explicit-address", func(p *packetNamespacePolicy) { p.Listeners = []string{"192.0.2.1:30000"} }},
		{"hostname", func(p *packetNamespacePolicy) { p.Listeners = []string{"localhost:30000"} }},
		{"duplicate", func(p *packetNamespacePolicy) { p.Listeners = []string{"[::]:30000", "[::]:30000"} }},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			modified := policy
			test.change(&modified)
			write(modified, 0600)
			if _, err := readPacketNamespacePolicy(path); err == nil {
				t.Fatal("unsafe policy accepted")
			}
		})
	}
	write(policy, 0644)
	if _, err := readPacketNamespacePolicy(path); err == nil {
		t.Fatal("public policy accepted")
	}
	write(policy, 0600)
	link := filepath.Join(dir, "link.json")
	if err := os.Symlink(path, link); err != nil {
		t.Fatal(err)
	}
	if _, err := readPacketNamespacePolicy(link); err == nil {
		t.Fatal("symlink policy accepted")
	}
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(path, append(data, []byte(" {}")...), 0600); err != nil {
		t.Fatal(err)
	}
	if _, err := readPacketNamespacePolicy(path); err == nil {
		t.Fatal("second JSON value accepted")
	}
}
