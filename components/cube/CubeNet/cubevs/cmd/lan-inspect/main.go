// Copyright (c) 2026 Brocade contributors.
// SPDX-License-Identifier: Apache-2.0
package main

import (
	"encoding/json"
	"fmt"
	"github.com/tencentcloud/CubeSandbox/CubeNet/cubevs"
	"os"
	"strconv"
)

func main() {
	if len(os.Args) != 2 {
		fmt.Fprintln(os.Stderr, "usage: lan-inspect IFINDEX")
		os.Exit(2)
	}
	index, err := strconv.ParseUint(os.Args[1], 10, 32)
	if err != nil || index == 0 {
		fmt.Fprintln(os.Stderr, "invalid ifindex")
		os.Exit(2)
	}
	result, err := cubevs.InspectLANEndpoint(uint32(index))
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
	if err := json.NewEncoder(os.Stdout).Encode(result); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}
