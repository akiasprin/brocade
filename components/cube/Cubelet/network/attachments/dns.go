// Copyright (c) 2026 Brocade contributors.
// SPDX-License-Identifier: Apache-2.0
package attachments

import (
	"errors"
	"net/netip"
	"strings"
)

// Kata may inject resolv.conf as a read-only bind. Overlay an instance-owned
// file instead of remounting/writing the engine's file. Keep its inode stable:
// repeated convergence must not accumulate another mount on every poll.
func dnsCommands(servers []string) ([]string, error) {
	if len(servers) > 4 {
		return nil, errors.New("too many container DNS servers")
	}
	lines := make([]string, 0, len(servers))
	for _, server := range servers {
		ip, err := netip.ParseAddr(server)
		if err != nil || ip.IsUnspecified() || ip.IsMulticast() || ip.IsLoopback() {
			return nil, errors.New("invalid container DNS server")
		}
		lines = append(lines, "nameserver "+ip.String())
	}
	const directory = "/etc/brocade"
	const original = directory + "/network-dns-original"
	const managed = directory + "/network-dns-managed"
	const target = "/etc/resolv.conf"
	bind := "chmod 0644 " + managed + "; if [ ! " + managed + " -ef " + target + " ]; then mount --bind " + managed + " " + target + "; fi"
	// Original/managed paths are inside a private directory, and may never be
	// symlinks. Only Guest-local files and mount objects are changed.
	prepare := "umask 077; test ! -L " + directory + "; mkdir -p " + directory + "; test ! -L " + original + "; test ! -L " + managed
	if len(lines) == 0 {
		return []string{"if [ -f " + original + " ]; then " + prepare + "; cat " + original + " > " + managed + "; " + bind + "; rm " + original + "; fi"}, nil
	}
	return []string{
		prepare + "; if [ ! -e " + original + " ]; then cp -pL " + target + " " + original + "; fi",
		"printf %s " + shellWord(strings.Join(lines, "\n")+"\n") + " > " + managed,
		bind,
	}, nil
}
