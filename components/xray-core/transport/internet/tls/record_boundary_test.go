package tls

import (
	"bytes"
	"io"
	"net"
	"testing"
)

func TestTLSRecordBoundaryConnStopsAtRecordEdges(t *testing.T) {
	client, server := net.Pipe()
	defer client.Close()
	defer server.Close()

	wire := []byte{
		23, 3, 3, 0, 3, 'o', 'n', 'e',
		23, 3, 3, 0, 3, 't', 'w', 'o',
	}
	go func() {
		_, _ = client.Write(wire)
	}()

	conn := newTLSRecordBoundaryConn(server)
	buffer := make([]byte, len(wire))
	wantParts := [][]byte{wire[:5], wire[5:8], wire[8:13], wire[13:]}
	for index, want := range wantParts {
		n, err := conn.Read(buffer)
		if err != nil {
			t.Fatalf("read %d: %v", index, err)
		}
		if !bytes.Equal(buffer[:n], want) {
			t.Fatalf("read %d = %v, want %v", index, buffer[:n], want)
		}
	}
	if err := client.Close(); err != nil {
		t.Fatal(err)
	}
	if _, err := conn.Read(buffer); err != io.EOF {
		t.Fatalf("final read error = %v, want EOF", err)
	}
}
