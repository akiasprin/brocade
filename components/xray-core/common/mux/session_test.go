package mux_test

import (
	"fmt"
	"testing"

	. "github.com/xtls/xray-core/common/mux"
)

func TestSessionIDsDoNotWrapAtUint16Boundary(t *testing.T) {
	// Exercise both the supported pool ceiling and direct callers without one.
	for _, limit := range []uint32{65535, 65536, 0} {
		t.Run(fmt.Sprint(limit), func(t *testing.T) {
			m := NewSessionManager()
			strategy := &ClientStrategy{MaxConcurrency: 2, MaxConnection: limit}
			first := m.Allocate(strategy)
			if first == nil || first.ID != 1 {
				t.Fatal("first session was not allocated")
			}
			// Keep ID 1 active while the other slot reaches the final ID.
			for id := 2; id <= 65535; id++ {
				s := m.Allocate(strategy)
				if s == nil || int(s.ID) != id {
					t.Fatalf("session %d was not allocated with its expected ID", id)
				}
				m.Remove(false, s.ID)
			}
			if s := m.Allocate(strategy); s != nil {
				t.Fatalf("exhausted IDs wrapped to %d", s.ID)
			}
			if m.Count() != 65535 || m.Size() != 1 {
				t.Fatalf("ID exhaustion damaged existing sessions: count=%d size=%d", m.Count(), m.Size())
			}
			m.Remove(false, first.ID)
			if !m.CloseIfNoSessionAndIdle(0, 65535) {
				t.Fatal("exhausted worker did not close after its final active session")
			}
		})
	}
}

func TestSessionManagerAdd(t *testing.T) {
	m := NewSessionManager()

	s := m.Allocate(&ClientStrategy{})
	if s.ID != 1 {
		t.Error("id: ", s.ID)
	}
	if m.Size() != 1 {
		t.Error("size: ", m.Size())
	}

	s = m.Allocate(&ClientStrategy{})
	if s.ID != 2 {
		t.Error("id: ", s.ID)
	}
	if m.Size() != 2 {
		t.Error("size: ", m.Size())
	}

	s = &Session{
		ID: 4,
	}
	m.Add(s)
	if s.ID != 4 {
		t.Error("id: ", s.ID)
	}
	if m.Size() != 3 {
		t.Error("size: ", m.Size())
	}
}

func TestSessionManagerClose(t *testing.T) {
	m := NewSessionManager()
	s := m.Allocate(&ClientStrategy{})

	if m.CloseIfNoSessionAndIdle(m.Size(), m.Count()) {
		t.Error("able to close")
	}
	m.Remove(false, s.ID)
	if !m.CloseIfNoSessionAndIdle(m.Size(), m.Count()) {
		t.Error("not able to close")
	}
}
