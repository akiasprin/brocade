package anytls

import (
	"testing"

	"github.com/xtls/xray-core/common/platform"
)

func TestFramedDownlinkSpliceIsExplicitlyOptIn(t *testing.T) {
	tests := []struct {
		value string
		want  bool
	}{
		{value: "", want: false},
		{value: "off", want: false},
		{value: "disabled", want: false},
		{value: "auto", want: false},
		{value: "unexpected", want: false},
		{value: "1", want: true},
		{value: "true", want: true},
		{value: "on", want: true},
		{value: "enable", want: true},
		{value: "enabled", want: true},
	}
	for _, test := range tests {
		t.Run(test.value, func(t *testing.T) {
			t.Setenv(platform.UseAnyTLSSplice, test.value)
			if got := framedDownlinkSpliceEnabled(); got != test.want {
				t.Fatalf("framedDownlinkSpliceEnabled() = %v, want %v", got, test.want)
			}
		})
	}
}

func TestKernelTLSVectoredWriteDefaultsToAuto(t *testing.T) {
	tests := []struct {
		value string
		want  bool
	}{
		{value: "", want: true},
		{value: "auto", want: true},
		{value: "on", want: true},
		{value: "1", want: true},
		{value: "unexpected", want: false},
		{value: "off", want: false},
		{value: "disabled", want: false},
		{value: "0", want: false},
	}
	for _, test := range tests {
		t.Run(test.value, func(t *testing.T) {
			t.Setenv(platform.UseAnyTLSWritev, test.value)
			if got := kernelTLSVectoredWriteEnabled(); got != test.want {
				t.Fatalf("kernelTLSVectoredWriteEnabled() = %v, want %v", got, test.want)
			}
		})
	}
}
