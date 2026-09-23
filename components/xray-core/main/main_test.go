//go:build coveragemain
// +build coveragemain

package main

import (
	"flag"
	"os"
	"testing"
)

func TestRunMainForCoverage(t *testing.T) {
	// The generated test main parses testing flags before entering this test.
	// Keep Xray's command and flags after the first positional argument, then
	// expose only that tail to the real CLI entry point.
	os.Args = append([]string{os.Args[0]}, flag.Args()...)
	main()
}
