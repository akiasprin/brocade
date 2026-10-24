package sourceaccess

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strconv"
	"strings"
)

type receipt struct {
	Revision   int64  `json:"revision"`
	Healthy    bool   `json:"healthy"`
	StartTicks uint64 `json:"pid_start_ticks"`
	BootID     string `json:"boot_id"`
}

func (m *Manager) receiptPath() string { return fmt.Sprintf("%s.%d.json", m.path, os.Getpid()) }

// Receipts are observations, not durable authority. PID start ticks and boot ID
// prevent an abandoned file from confirming a different process after restart.
func (m *Manager) writeReceipt(revision int64, healthy bool) error {
	stat, err := os.ReadFile("/proc/self/stat")
	if err != nil {
		return err
	}
	_, rest, ok := strings.Cut(string(stat), ") ")
	fields := strings.Fields(rest)
	if !ok || len(fields) < 20 {
		return fmt.Errorf("process identity unavailable")
	}
	ticks, err := strconv.ParseUint(fields[19], 10, 64)
	if err != nil {
		return err
	}
	boot, err := os.ReadFile("/proc/sys/kernel/random/boot_id")
	if err != nil {
		return err
	}
	data, err := json.Marshal(receipt{revision, healthy, ticks, strings.TrimSpace(string(boot))})
	if err != nil {
		return err
	}
	f, err := os.CreateTemp(filepath.Dir(m.path), ".source-access-receipt-*")
	if err != nil {
		return err
	}
	defer os.Remove(f.Name())
	_, err = f.Write(data)
	closeErr := f.Close()
	if err != nil {
		return err
	}
	if closeErr != nil {
		return closeErr
	}
	return os.Rename(f.Name(), m.receiptPath())
}
