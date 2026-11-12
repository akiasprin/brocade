//go:build linux

package tcp

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"golang.org/x/sys/unix"
)

func largeExportControl(t *testing.T, count int) (*tcpHandoff, *net.TCPListener, handoffGate) {
	t.Helper()
	dir := t.TempDir()
	if err := os.Chmod(dir, 0700); err != nil {
		t.Fatal(err)
	}
	manifest := handoffManifest{Version: 1, GenerationID: "large-export-generation-0001", ControlPath: filepath.Join(dir, "control.sock")}
	listeners := make([]*net.TCPListener, count)
	for i := range listeners {
		listeners[i] = handoffTestListener(t)
		manifest.Listeners = append(manifest.Listeners, handoffEndpoint{Address: listeners[i].Addr().String(), Protocol: unix.IPPROTO_TCP})
	}
	c := handoffTestControl(t, manifest)
	var firstGate handoffGate
	for i, l := range listeners {
		_, gate := handoffTestOpen(t, c, l)
		if i == 0 {
			firstGate = gate
		}
	}
	if err := c.markReady(); err != nil {
		t.Fatal(err)
	}
	if err := c.seal(); err != nil {
		t.Fatal(err)
	}
	return c, listeners[0], firstGate
}

func readLargeExport(t *testing.T, c *tcpHandoff, operation string) (handoffResponse, []int, int) {
	t.Helper()
	conn, err := net.DialUnix("unixpacket", nil, &net.UnixAddr{Name: c.manifest.ControlPath, Net: "unixpacket"})
	if err != nil {
		t.Fatal(err)
	}
	defer conn.Close()
	if err := conn.SetDeadline(time.Now().Add(5 * time.Second)); err != nil {
		t.Fatal(err)
	}
	body, _ := json.Marshal(handoffRequest{Operation: operation, GenerationID: c.manifest.GenerationID, Epoch: 1, Intent: "large-export-intent-0001"})
	if _, err := conn.Write(body); err != nil {
		t.Fatal(err)
	}
	var first handoffResponse
	var fds []int
	t.Cleanup(func() {
		for _, fd := range fds {
			unix.Close(fd)
		}
	})
	for packets := 1; packets <= 3; packets++ {
		data, ancillary := make([]byte, handoffMaxMessage+1), make([]byte, unix.CmsgSpace(handoffMaxFDsPerMessage*4))
		n, on, flags, _, err := conn.ReadMsgUnix(data, ancillary)
		if err != nil || flags&(unix.MSG_TRUNC|unix.MSG_CTRUNC) != 0 {
			t.Fatalf("read export: %v flags=%d", err, flags)
		}
		var frame handoffResponse
		if err := json.Unmarshal(data[:n], &frame); err != nil {
			t.Fatal(err)
		}
		if packets == 1 {
			first = frame
		} else if frame.Status != nil {
			t.Fatal("continuation repeated the status snapshot")
		}
		if frame.Error != "" {
			return frame, fds, packets
		}
		if operation == "export_batched" && (frame.FDExport == nil || frame.FDExport.Offset != len(fds)) {
			t.Fatalf("wrong batch offset: %+v, already received %d", frame.FDExport, len(fds))
		}
		messages, err := unix.ParseSocketControlMessage(ancillary[:on])
		if err != nil {
			t.Fatal(err)
		}
		for _, message := range messages {
			rights, err := unix.ParseUnixRights(&message)
			if err != nil {
				t.Fatal(err)
			}
			fds = append(fds, rights...)
		}
		if operation == "export" || len(fds) == frame.FDExport.Total {
			return first, fds, packets
		}
	}
	t.Fatal("export exceeded three bounded packets")
	return first, fds, 0
}

func TestHandoffFDExportBatchesPreserveListenersAndAcceptedConnection(t *testing.T) {
	for _, count := range []int{253, 254, 512} {
		t.Run(fmt.Sprint(count), func(t *testing.T) {
			c, first, gate := largeExportControl(t, count)
			serveHandoffEcho(t, first, gate, 'a')
			if err := c.resume(); err != nil {
				t.Fatal(err)
			}
			client, err := net.DialTimeout("tcp", first.Addr().String(), time.Second)
			if err != nil {
				t.Fatal(err)
			}
			defer client.Close()
			if err := client.SetDeadline(time.Now().Add(5 * time.Second)); err != nil {
				t.Fatal(err)
			}
			checkEcho := func() {
				t.Helper()
				if _, err := client.Write([]byte("x")); err != nil {
					t.Fatal(err)
				}
				buf := make([]byte, 2)
				if _, err := io.ReadFull(client, buf); err != nil || string(buf) != "ax" {
					t.Fatalf("accepted connection lost: %q %v", buf, err)
				}
			}
			checkEcho()
			ctx, cancel := context.WithTimeout(context.Background(), time.Second)
			defer cancel()
			if err := c.pause(ctx); err != nil {
				t.Fatal(err)
			}
			response, fds, packets := readLargeExport(t, c, "export_batched")
			if response.Error != "" || response.Status == nil || !response.Status.FDExportBatches || len(fds) != count || packets != (count+252)/253 {
				t.Fatalf("incomplete export: response=%+v fds=%d packets=%d", response, len(fds), packets)
			}
			for i, fd := range fds {
				identity, err := inspectHandoffFD(fd)
				if err != nil || identity != response.Status.Listeners[i].handoffSocketIdentity {
					t.Fatalf("descriptor %d differs from ordered snapshot: %+v %v", i, identity, err)
				}
			}
			checkEcho()
		})
	}
}

func TestHandoffLegacyFDExportReportsLimitWithoutEOF(t *testing.T) {
	for _, count := range []int{253, 254} {
		t.Run(fmt.Sprint(count), func(t *testing.T) {
			c, _, _ := largeExportControl(t, count)
			response, fds, packets := readLargeExport(t, c, "export")
			if packets != 1 || response.FDExport != nil {
				t.Fatal("legacy response framing changed")
			}
			if count == 253 {
				if response.Error != "" || len(fds) != count {
					t.Fatalf("legacy export failed: %+v fds=%d", response, len(fds))
				}
			} else if !strings.Contains(response.Error, "export_batched") || len(fds) != 0 {
				t.Fatalf("missing explicit legacy limit error: %+v fds=%d", response, len(fds))
			}
			if err := c.resume(); err != nil {
				t.Fatalf("old generation cannot resume: %v", err)
			}
		})
	}
}
