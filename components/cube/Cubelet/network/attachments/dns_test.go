// Copyright (c) 2026 Brocade contributors.
// SPDX-License-Identifier: Apache-2.0
package attachments

import (
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
)

func TestDNSRejectsInvalidInputs(t *testing.T) {
	for _, servers := range [][]string{{"127.0.0.1"}, {"::"}, {"224.0.0.1"}, {"example.org"}, {"1.1.1.1", "1.1.1.1", "1.1.1.1", "1.1.1.1", "1.1.1.1"}} {
		if _, err := dnsCommands(servers); err == nil {
			t.Fatalf("invalid DNS accepted: %v", servers)
		}
	}
}
func TestDNSReadOnlyBindOverrideRepeatedApplyAndRestore(t *testing.T) {
	if os.Getenv("BROCADE_TEST_NETNS") != "1" {
		t.Skip("requires explicit private user/mount namespace")
	}
	root := t.TempDir()
	etc := filepath.Join(root, "etc")
	if err := os.Mkdir(etc, 0700); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(etc, "resolv.conf"), nil, 0644); err != nil {
		t.Fatal(err)
	}
	if err := os.Symlink("/proc/self/mounts", filepath.Join(etc, "mtab")); err != nil {
		t.Fatal(err)
	}
	original := filepath.Join(root, "original")
	if err := os.WriteFile(original, []byte("nameserver 192.0.2.53\n"), 0644); err != nil {
		t.Fatal(err)
	}
	first, err := dnsCommands([]string{"1.1.1.1", "2606:4700:4700::1111"})
	if err != nil {
		t.Fatal(err)
	}
	changed, err := dnsCommands([]string{"9.9.9.9"})
	if err != nil {
		t.Fatal(err)
	}
	restore, err := dnsCommands(nil)
	if err != nil {
		t.Fatal(err)
	}
	script := "set -eu; mount --make-rprivate /; mount --bind " + shellWord(etc) + " /etc; mount --bind " + shellWord(original) + " /etc/resolv.conf; mount -o remount,bind,ro /etc/resolv.conf; "
	script += "if sh -c 'printf wrong > /etc/resolv.conf' 2>/dev/null; then exit 21; fi; "
	script += strings.Join(first, "; ") + "; grep -Fx 'nameserver 1.1.1.1' /etc/resolv.conf; test /etc/brocade/network-dns-managed -ef /etc/resolv.conf; "
	script += "before=$(wc -l </proc/self/mountinfo); " + strings.Join(first, "; ") + "; test \"$(wc -l </proc/self/mountinfo)\" = \"$before\"; "
	script += strings.Join(changed, "; ") + "; grep -Fx 'nameserver 9.9.9.9' /etc/resolv.conf; test \"$(stat -c %a /etc/brocade/network-dns-managed)\" = 644; "
	script += strings.Join(restore, "; ") + "; cmp /etc/resolv.conf " + shellWord(original) + "; test ! -e /etc/brocade/network-dns-original; "
	script += strings.Join(restore, "; ") + "; cmp /etc/resolv.conf " + shellWord(original)
	command := exec.Command("unshare", "--user", "--map-root-user", "--net", "--mount", "--", "/bin/sh", "-c", script)
	if output, err := command.CombinedOutput(); err != nil {
		t.Fatalf("DNS override/restore failed: %v\n%s", err, output)
	}
}
