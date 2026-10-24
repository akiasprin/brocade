// Package testutil exercises the real file watcher used by every protocol.
package testutil

import (
	"context"
	"encoding/json"
	"fmt"
	"net"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/xtls/xray-core/common/sourceaccess"
)

type PeerConn struct {
	net.Conn
	IP string
}

func (c PeerConn) RemoteAddr() net.Addr { return &net.TCPAddr{IP: net.ParseIP(c.IP), Port: 4321} }

func Open(t *testing.T) (context.Context, func(sourceaccess.Policy)) {
	t.Helper()
	path := filepath.Join(t.TempDir(), "policy.json")
	write := func(policy sourceaccess.Policy) {
		t.Helper()
		data, err := json.Marshal(policy)
		if err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(path+".new", data, 0600); err != nil {
			t.Fatal(err)
		}
		if err := os.Rename(path+".new", path); err != nil {
			t.Fatal(err)
		}
	}
	write(sourceaccess.Policy{Revision: 1})
	m, err := sourceaccess.New(path)
	if err != nil {
		t.Fatal(err)
	}
	if err := m.Start(); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = m.Close() })
	update := func(policy sourceaccess.Policy) {
		t.Helper()
		write(policy)
		deadline := time.Now().Add(3 * time.Second)
		for time.Now().Before(deadline) {
			data, _ := os.ReadFile(fmt.Sprintf("%s.%d.json", path, os.Getpid()))
			var receipt struct {
				Revision int64 `json:"revision"`
				Healthy  bool  `json:"healthy"`
			}
			if json.Unmarshal(data, &receipt) == nil && receipt.Healthy && receipt.Revision == policy.Revision {
				return
			}
			time.Sleep(10 * time.Millisecond)
		}
		t.Fatal("source access watcher did not confirm policy")
	}
	return sourceaccess.Context(context.Background(), m), update
}
