package mtproto

import (
	"bytes"
	"context"
	"crypto/cipher"
	"encoding/binary"
	"io"
	"net"
	"testing"
	"time"

	handlers "github.com/xtls/xray-core/app/proxyman/command"
	statscmd "github.com/xtls/xray-core/app/stats/command"
	"github.com/xtls/xray-core/common/protocol"
	"github.com/xtls/xray-core/common/serial"
	"google.golang.org/grpc"
)

type mtReviewClient struct {
	conn    net.Conn
	encrypt cipher.Stream
	reader  io.Reader
}

func mtReviewOpen(t *testing.T, dial func() net.Conn, secret [16]byte, nonce byte) mtReviewClient {
	t.Helper()
	c := dial()
	header, encrypt, decrypt := testClientHeader(secret, [4]byte{0xdd, 0xdd, 0xdd, 0xdd}, 2)
	// This unused clear prefix byte varies the replay digest without changing
	// the key material at bytes 8..55 or the encrypted transport/DC fields.
	header[0] ^= nonce
	if _, err := c.Write(header); err != nil {
		t.Fatal(err)
	}
	return mtReviewClient{c, encrypt, &cipher.StreamReader{S: decrypt, R: c}}
}
func (c mtReviewClient) exchange(t *testing.T, value byte) int64 {
	t.Helper()
	_ = c.conn.SetDeadline(time.Now().Add(3 * time.Second))
	payload := bytes.Repeat([]byte{value}, 56)
	plain := binaryLittleFrame(payload)
	encoded := make([]byte, len(plain))
	c.encrypt.XORKeyStream(encoded, plain)
	if _, err := c.conn.Write(encoded); err != nil {
		t.Fatal(err)
	}
	var length [4]byte
	if _, err := io.ReadFull(c.reader, length[:]); err != nil {
		t.Fatal(err)
	}
	n := int(binary.LittleEndian.Uint32(length[:]))
	if n < 56 || n > 59 {
		t.Fatalf("unexpected framed response length %d", n)
	}
	body := make([]byte, n)
	if _, err := io.ReadFull(c.reader, body); err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(body[:56], payload) {
		t.Fatal("middle RPC crossed user/connection payloads")
	}
	return int64(n + 4)
}
func frozenMTRevocation(t *testing.T, channel *grpc.ClientConn, dial func() net.Conn, alice net.Conn, stats statscmd.StatsServiceClient) {
	t.Helper()
	var bobSecret, aliceSecret [16]byte
	for i := range bobSecret {
		bobSecret[i] = byte(32 + i)
		aliceSecret[i] = byte(i)
	}
	bob := mtReviewOpen(t, dial, bobSecret, 1)
	bobDown := bob.exchange(t, 0x62)
	mutate := func(operation *serial.TypedMessage) {
		ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
		defer cancel()
		if _, err := handlers.NewHandlerServiceClient(channel).AlterInbound(ctx, &handlers.AlterInboundRequest{Tag: "mt-entry", Operation: operation}); err != nil {
			t.Fatal(err)
		}
	}
	mutate(serial.ToTypedMessage(&handlers.RemoveUserOperation{Email: "alice@tenant#entry"}))
	_ = alice.SetReadDeadline(time.Now().Add(time.Second))
	mtFrozenRequireClosed(t, alice, "revoked user's original carrier")
	denied := mtReviewOpen(t, dial, aliceSecret, 1)
	mtFrozenRequireClosed(t, denied.conn, "revoked credential")
	bobDown += bob.exchange(t, 0x63)
	mutate(serial.ToTypedMessage(&handlers.AddUserOperation{User: &protocol.User{Email: "alice@tenant#entry", Account: serial.ToTypedMessage(&Account{Secret: "00010203-0405-0607-0809-0a0b0c0d0e0f"})}}))
	restored := mtReviewOpen(t, dial, aliceSecret, 2)
	aliceDown := restored.exchange(t, 0x61)
	bobDown += bob.exchange(t, 0x64)
	// The first Alice exchange happened before this helper and must remain in
	// the same cumulative ledger after RemoveUser/AddUser.
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()
	snapshot, err := stats.QueryStats(ctx, &statscmd.QueryStatsRequest{Pattern: "user>>>"})
	if err != nil {
		t.Fatal(err)
	}
	values := map[string]int64{}
	for _, s := range snapshot.Stat {
		values[s.Name] = s.Value
	}
	if values["user>>>alice@tenant#entry>>>traffic>>>uplink"] != 120 || values["user>>>bob@tenant#entry>>>traffic>>>uplink"] != 180 {
		t.Fatalf("remove/add reset or mixed user totals: %+v", values)
	}
	if values["user>>>bob@tenant#entry>>>traffic>>>downlink"] != bobDown || values["user>>>alice@tenant#entry>>>traffic>>>downlink"] <= aliceDown {
		t.Fatal("downlink counter identity changed across revocation")
	}
	t.Log("RemoveUser closed Alice and denied new Alice; Bob kept the original socket; AddUser restored only a new carrier; cumulative counters survived")
}
func TestBrocadeFrozenMTProxyRevocationIsolation(t *testing.T) {
	t.Setenv("BROCADE_MT_TEST_LIFECYCLE", "1")
	TestBrocadeFrozenMTProxyDirectoryRPCAccountingAndBan(t)
}
