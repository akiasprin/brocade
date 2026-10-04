package utils

import "testing"

type privateFieldFixture struct {
	hidden int
}

func TestTryAccessFieldPreservesFieldAddress(t *testing.T) {
	fixture := &privateFieldFixture{hidden: 1}
	field, ok := TryAccessField[int](fixture, "hidden")
	if !ok {
		t.Fatal("TryAccessField() did not resolve an addressable private field")
	}
	*field = 2
	if fixture.hidden != 2 {
		t.Fatalf("private field = %d, want 2", fixture.hidden)
	}
}

func TestTryAccessFieldRejectsLayoutMismatch(t *testing.T) {
	fixture := &privateFieldFixture{}
	if field, ok := TryAccessField[string](fixture, "hidden"); ok || field != nil {
		t.Fatalf("wrong field type resolved as (%v, %v)", field, ok)
	}
	if field, ok := TryAccessField[int](fixture, "missing"); ok || field != nil {
		t.Fatalf("missing field resolved as (%v, %v)", field, ok)
	}
}
