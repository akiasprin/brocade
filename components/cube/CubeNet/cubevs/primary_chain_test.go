// Copyright (c) 2026 Brocade contributors.
// SPDX-License-Identifier: Apache-2.0
package cubevs

import (
	"errors"
	"net"
	"os"
	"testing"

	"github.com/cilium/ebpf"
)

func TestPrimaryChainIPv6Identity(t *testing.T) {
	got, err := PrimaryChainIPv6(net.ParseIP("192.168.3.9"))
	if err != nil || got.String() != "fd68:bcad:0:1::c0a8:309" {
		t.Fatalf("address: %s, %v", got, err)
	}
	if _, err = PrimaryChainIPv6(net.ParseIP("2001:db8::1")); err == nil {
		t.Fatal("IPv6 must not invent an unrelated delivery identity")
	}
}

// Run in the disposable Ubuntu KVM, not on a developer's production BPF pins.
// Every map is anonymous; this checks the actual kernel verifier without attaching a program.
func TestPrimaryChainProgramsLoadOnKernel(t *testing.T) {
	if os.Getenv("BROCADE_CUBE_KVM_VERIFY") != "1" {
		t.Skip("requires explicit disposable KVM kernel verification")
	}
	for _, source := range []struct {
		name string
		load func() (*ebpf.CollectionSpec, error)
	}{{"primary", loadMvmtap}, {"reply", loadLocalgw}} {
		t.Run(source.name, func(t *testing.T) {
			spec, err := source.load()
			if err != nil {
				t.Fatal(err)
			}
			for _, m := range spec.Maps {
				m.Pinning = ebpf.PinNone
			}
			collection, err := ebpf.NewCollection(spec)
			if err != nil {
				var verifier *ebpf.VerifierError
				if errors.As(err, &verifier) {
					t.Logf("%+v", verifier)
				}
				t.Fatalf("kernel rejected %s programs: %v", source.name, err)
			}
			defer collection.Close()
			if collection.Maps[primaryChainMap] == nil {
				t.Fatal("primary route map is missing")
			}
		})
	}
}
