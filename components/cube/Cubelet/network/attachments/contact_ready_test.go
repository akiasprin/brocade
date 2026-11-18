// Copyright (c) 2026 Brocade contributors.
// SPDX-License-Identifier: Apache-2.0
package attachments

import (
	"encoding/json"
	"os"
	"os/exec"
	"testing"
	"time"
)

func TestContactAddressReadiness(t *testing.T) {
	for _, test := range []struct {
		name          string
		address       object
		ready, failed bool
	}{
		{"ready", object{"local": "fd00::2"}, true, false},
		{"tentative", object{"tentative": true}, false, false},
		{"legacy-flags", object{"flags": []any{"tentative"}}, false, false},
		{"failed", object{"dadfailed": true}, false, true},
	} {
		t.Run(test.name, func(t *testing.T) {
			ready, err := contactAddressesReady([]object{{"ifname": "eth0", "addr_info": []any{test.address}}}, "eth0")
			if ready != test.ready || (err != nil) != test.failed {
				t.Fatalf("ready=%v err=%v", ready, err)
			}
		})
	}
}

func TestContactAddressDADKernelWait(t *testing.T) {
	if os.Getenv("BROCADE_DAD_HELPER") != "1" {
		if os.Getenv("BROCADE_TEST_NETNS") != "1" {
			t.Skip("requires explicit private network namespace")
		}
		cmd := exec.Command("unshare", "--user", "--map-root-user", "--net", "--", os.Args[0], "-test.run=^TestContactAddressDADKernelWait$", "-test.v")
		cmd.Env = append(os.Environ(), "BROCADE_DAD_HELPER=1")
		if out, err := cmd.CombinedOutput(); err != nil {
			t.Fatalf("DAD kernel proof: %v\n%s", err, out)
		}
		return
	}
	for _, args := range [][]string{{"link", "set", "lo", "up"}, {"link", "add", "test0", "type", "veth", "peer", "name", "test1"}, {"link", "set", "test0", "up"}, {"link", "set", "test1", "up"}, {"-6", "address", "add", "fd00::2/64", "dev", "test0"}} {
		if out, err := exec.Command("ip", args...).CombinedOutput(); err != nil {
			t.Fatalf("fixture: %v %s", err, out)
		}
	}
	read := func() bool {
		out, err := exec.Command("ip", "-j", "address", "show", "dev", "test0").Output()
		if err != nil {
			t.Fatal(err)
		}
		var devices []object
		if err = json.Unmarshal(out, &devices); err != nil {
			t.Fatal(err)
		}
		ready, err := contactAddressesReady(devices, "test0")
		if err != nil {
			t.Fatal(err)
		}
		return ready
	}
	if read() {
		t.Fatal("fixture did not expose tentative address")
	}
	args := []string{"-6", "route", "replace", "fd00::/64", "dev", "test0", "src", "fd00::2", "table", "30000"}
	if exec.Command("ip", args...).Run() == nil {
		t.Fatal("kernel unexpectedly accepted tentative source")
	}
	until := time.Now().Add(5 * time.Second)
	for !read() {
		if time.Now().After(until) {
			t.Fatal("DAD did not finish")
		}
		time.Sleep(25 * time.Millisecond)
	}
	if out, err := exec.Command("ip", args...).CombinedOutput(); err != nil {
		t.Fatalf("ready source rejected: %v %s", err, out)
	}
}
