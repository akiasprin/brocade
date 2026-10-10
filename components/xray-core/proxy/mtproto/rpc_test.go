package mtproto

import (
	"context"
	"encoding/binary"
	"encoding/hex"
	"net"
	"os"
	"testing"
	"time"
)

func TestMiddleRPCKeysMatchOfficialLayout(t *testing.T) {
	var serverNonce, clientNonce [16]byte
	for index := range serverNonce {
		serverNonce[index] = byte(index)
		clientNonce[index] = byte(index + 16)
	}
	secret := make([]byte, 128)
	for index := range secret {
		secret[index] = byte(index)
	}
	writeKey, writeIV, readKey, readIV, err := middleRPCKeys(
		serverNonce,
		clientNonce,
		1_700_000_000,
		&net.TCPAddr{IP: net.ParseIP("198.51.100.20"), Port: 8888},
		&net.TCPAddr{IP: net.ParseIP("203.0.113.44"), Port: 50123},
		secret,
	)
	if err != nil {
		t.Fatal(err)
	}
	assertHex := func(name string, got []byte, want string) {
		t.Helper()
		if hex.EncodeToString(got) != want {
			t.Fatalf("%s = %x", name, got)
		}
	}
	assertHex("write key", writeKey, "4ba67f8f0f4fb40972be0220c165a1fc0a8ef29c26122f2ed51b624a30942fe6")
	assertHex("write iv", writeIV, "e0f4b7337e846e240e0af4cd69146f1a")
	assertHex("read key", readKey, "c7c5f8fe9c9967a2806c44fa052a4db5809d9481030549ae5903ef3d03d6f838")
	assertHex("read iv", readIV, "68eeed03952d03de8f827b1df456233f")
}

func TestMiddleRPCKeysRejectNATSocketSources(t *testing.T) {
	var serverNonce, clientNonce [16]byte
	secret := make([]byte, 128)
	server := &net.TCPAddr{IP: net.ParseIP("149.154.161.144"), Port: 8888}
	for _, source := range []string{"10.0.0.2", "192.168.1.170", "100.64.0.9"} {
		_, _, _, _, err := middleRPCKeys(
			serverNonce,
			clientNonce,
			1_700_000_000,
			server,
			&net.TCPAddr{IP: net.ParseIP(source), Port: 50123},
			secret,
		)
		if err == nil {
			t.Fatalf("NAT source %s was accepted", source)
		}
	}
}

func TestRPCFrameMatchesOfficialFullTransport(t *testing.T) {
	payload := []byte{0xaa, 0x87, 0xcb, 0x7a, 1, 2, 3, 4}
	frame := rpcFrame(-2, payload)
	if got, want := hex.EncodeToString(frame), "14000000feffffffaa87cb7a0102030443512be8"; got != want {
		t.Fatalf("frame = %s, want %s", got, want)
	}
	sequence, decoded, err := parseRPCFrame(frame)
	if err != nil {
		t.Fatal(err)
	}
	if sequence != -2 || hex.EncodeToString(decoded) != hex.EncodeToString(payload) {
		t.Fatalf("decoded sequence=%d payload=%x", sequence, decoded)
	}
}

func TestProcessIDMatchingUsesOfficialWildcardDirection(t *testing.T) {
	actual := processID{ip: [4]byte{1, 2, 3, 4}, port: 8888, pid: 42, utime: 99}
	remoteSocket := processID{ip: actual.ip, port: actual.port}
	if !processIDMatches(actual, remoteSocket) {
		t.Fatal("official socket-address pattern did not match the complete sender process id")
	}
	if processIDMatches(processID{ip: [4]byte{4, 3, 2, 1}, port: 8888}, remoteSocket) {
		t.Fatal("a sender from another middle-proxy address was accepted")
	}
	if processIDMatches(remoteSocket, actual) {
		t.Fatal("wildcards were incorrectly taken from the actual process id")
	}
}

func TestMiddleRPCHandshakeAcceptsTheOfficialInternalSenderIdentity(t *testing.T) {
	localPID := processID{
		ip:    [4]byte{198, 51, 100, 7},
		pid:   42,
		utime: 1_700_000_000,
	}
	reply := make([]byte, 32)
	binary.LittleEndian.PutUint32(reply[:4], rpcHandshake)
	putProcessID(reply[8:20], processID{
		ip:    [4]byte{10, 0, 0, 9},
		port:  8_888,
		pid:   4_242,
		utime: 1_700_000_010,
	})
	putProcessID(reply[20:32], localPID)

	if err := validateMiddleRPCHandshakeReply(reply, localPID); err != nil {
		t.Fatalf("official middle-proxy reply was rejected: %v", err)
	}

	wrongPeer := localPID
	wrongPeer.pid++
	putProcessID(reply[20:32], wrongPeer)
	if err := validateMiddleRPCHandshakeReply(reply, localPID); err == nil {
		t.Fatal("middle-proxy reply for another local process was accepted")
	}
}

// This opt-in test is the compatibility gate against Telegram's live middle proxy. It is kept
// out of the default suite because it depends on Telegram's network and current service data.
func TestOfficialMiddleRPCHandshake(t *testing.T) {
	if os.Getenv("XRAY_TEST_LIVE_MTPROTO") != "1" {
		t.Skip("set XRAY_TEST_LIVE_MTPROTO=1 to test Telegram's live middle proxy")
	}
	ctx, cancel := context.WithTimeout(context.Background(), 20*time.Second)
	defer cancel()
	directory := newOfficialProxyDirectory()
	target, secret, err := directory.target(ctx, 2)
	if err != nil {
		t.Fatal(err)
	}
	client, err := dialMiddleRPC(ctx, target, secret)
	if err != nil {
		t.Fatal(err)
	}
	if err := client.Close(); err != nil {
		t.Fatal(err)
	}
}
