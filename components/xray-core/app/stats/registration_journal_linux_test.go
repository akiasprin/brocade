//go:build linux

package stats

import (
	"golang.org/x/sys/unix"
	"path/filepath"
	"sync"
	"testing"
)

func TestGetOrRegisterCounterUsesPersistentJournal(t *testing.T) {
	path := filepath.Join(t.TempDir(), "accounting.bin")
	journal, err := openMappedCounterJournal(path)
	if err != nil {
		t.Fatal(err)
	}
	manager := &Manager{counters: make(map[string]*Counter), journal: journal}
	const label = "user>>>registration@example.test>>>traffic>>>uplink"
	var workers sync.WaitGroup
	for range 32 {
		workers.Go(func() {
			counter, err := manager.GetOrRegisterCounter(label)
			if err != nil {
				t.Error(err)
				return
			}
			counter.Add(1)
		})
	}
	workers.Wait()
	if got := journal.Counters()[label]; got == nil || got.Value() != 32 {
		t.Fatalf("persistent counter=%v", got)
	}
	if err := journal.file.Close(); err != nil {
		t.Fatal(err)
	}
	if err := unix.Munmap(journal.data); err != nil {
		t.Fatal(err)
	}
	reopened, err := openMappedCounterJournal(path)
	if err != nil {
		t.Fatal(err)
	}
	defer reopened.file.Close()
	defer unix.Munmap(reopened.data)
	if got := reopened.Counters()[label]; got == nil || got.Value() != 32 {
		t.Fatalf("reopened counter=%v", got)
	}
}
