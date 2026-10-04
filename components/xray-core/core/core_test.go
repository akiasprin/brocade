package core

import (
	"slices"
	"testing"
)

func TestVersionStatementAdvertisesBrocadeCapabilities(t *testing.T) {
	want := "Brocade-Capabilities: anytls-ktls-splice anytls-ktls-writev secure-dokodemo-splice"
	if !slices.Contains(VersionStatement(), want) {
		t.Fatal("version statement does not advertise Brocade splice capabilities")
	}
}
