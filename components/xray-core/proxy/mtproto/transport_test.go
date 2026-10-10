package mtproto

import (
	"bufio"
	"bytes"
	"encoding/binary"
	"net"
	"testing"
)

func TestPaddedIntermediatePacketUsesOfficialLengthAndFlags(t *testing.T) {
	wire := binary.LittleEndian.AppendUint32(nil, rpcFlagQuickAck|11)
	wire = append(wire, 1, 2, 3, 4, 5, 6, 7, 8, 0xaa, 0xbb, 0xcc)
	transport := &clientTransport{
		typeTag: [4]byte{0xdd, 0xdd, 0xdd, 0xdd},
		reader:  bufio.NewReader(bytes.NewReader(wire)),
	}
	payload, flags, err := transport.readPacket()
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(payload, []byte{1, 2, 3, 4, 5, 6, 7, 8}) {
		t.Fatalf("payload = %x", payload)
	}
	if want := rpcFlagMedium | rpcFlagExtMode2 | rpcFlagQuickAck; flags != want {
		t.Fatalf("flags = %08x, want %08x", flags, want)
	}
}

func TestIntermediateResponseFraming(t *testing.T) {
	var wire bytes.Buffer
	transport := &clientTransport{typeTag: [4]byte{0xee, 0xee, 0xee, 0xee}, writer: &wire}
	if err := transport.writePacket([]byte{1, 2, 3, 4, 5, 6, 7, 8}); err != nil {
		t.Fatal(err)
	}
	if got, want := wire.Bytes(), []byte{8, 0, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8}; !bytes.Equal(got, want) {
		t.Fatalf("wire = %x, want %x", got, want)
	}
}

func TestProxyRequestEncodesOfficialIPv4MappedAddresses(t *testing.T) {
	payload := make([]byte, 56)
	binary.LittleEndian.PutUint64(payload[:8], 1)
	request, err := proxyRequest(
		42,
		rpcFlagMedium|rpcFlagExtMode2,
		&net.TCPAddr{IP: net.ParseIP("203.0.113.7"), Port: 45678},
		&net.TCPAddr{IP: net.ParseIP("192.0.2.9"), Port: 15443},
		payload,
	)
	if err != nil {
		t.Fatal(err)
	}
	if got := binary.LittleEndian.Uint32(request[:4]); got != rpcProxyRequest {
		t.Fatalf("operation = %08x", got)
	}
	if got := int64(binary.LittleEndian.Uint64(request[8:16])); got != 42 {
		t.Fatalf("connection id = %d", got)
	}
	wantRemote := []byte{0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xff, 203, 0, 113, 7}
	if !bytes.Equal(request[16:32], wantRemote) || binary.LittleEndian.Uint32(request[32:36]) != 45678 {
		t.Fatalf("remote address = %x port=%d", request[16:32], binary.LittleEndian.Uint32(request[32:36]))
	}
	if got := request[56:]; !bytes.Equal(got, payload) {
		t.Fatalf("payload = %x", got)
	}
}

func TestValidateOfficialTelegramHandshakeMethods(t *testing.T) {
	packet := make([]byte, 40)
	binary.LittleEndian.PutUint32(packet[16:20], 20)
	binary.LittleEndian.PutUint32(packet[20:24], 0xbe7e8ef1)
	flags, err := validateMTProtoRequest(packet)
	if err != nil || flags != rpcFlagUnencrypted {
		t.Fatalf("flags=%x err=%v", flags, err)
	}
	binary.LittleEndian.PutUint32(packet[20:24], 0xdeadbeef)
	if _, err := validateMTProtoRequest(packet); err == nil {
		t.Fatal("unknown unencrypted method was accepted")
	}
}
