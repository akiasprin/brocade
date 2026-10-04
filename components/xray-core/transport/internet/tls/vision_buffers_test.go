package tls

import (
	gotls "crypto/tls"
	"net"
	"testing"

	utls "github.com/refraction-networking/utls"
)

func TestVisionBuffersExposeTLSReadAhead(t *testing.T) {
	client, server := net.Pipe()
	defer client.Close()
	defer server.Close()

	conn := Client(client, &gotls.Config{InsecureSkipVerify: true}).(*Conn)
	input, rawInput := conn.VisionBuffers()
	if input == nil || rawInput == nil {
		t.Fatal("VisionBuffers() returned nil TLS buffers")
	}
}

func TestVisionBuffersExposeUTLSReadAhead(t *testing.T) {
	client, server := net.Pipe()
	defer client.Close()
	defer server.Close()

	fingerprint := utls.HelloChrome_Auto
	conn := UClient(client, &gotls.Config{InsecureSkipVerify: true}, &fingerprint).(*UConn)
	input, rawInput := conn.VisionBuffers()
	if input == nil || rawInput == nil {
		t.Fatal("VisionBuffers() returned nil uTLS buffers")
	}
}
