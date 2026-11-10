package cgroup

import (
	"reflect"
	"testing"
)

func TestWarmPoolDoesNotExpandToRecoveredHighID(t *testing.T) {
	for _, numa := range []bool{false, true} {
		high := MakeFullCgID(3000, numa, 0)
		want := []uint32{MakeFullCgID(0, numa, 0), high}
		if got := warmAndAssigned(1, []uint32{high}, numa, 0); !reflect.DeepEqual(got, want) {
			t.Fatalf("warm and recovered IDs = %v, want %v", got, want)
		}
	}
}
