package mtproto

import (
	"bytes"
	"crypto/cipher"
	"crypto/sha256"
	"encoding/binary"
	"encoding/hex"
	"testing"
	"time"

	xcrypto "github.com/xtls/xray-core/common/crypto"
	"github.com/xtls/xray-core/common/protocol"
)

// This fixture is produced from Telegram's published transport-obfuscation pseudocode with a
// deterministic 0..63 initialization payload, a 00..0f secret, padded-intermediate transport,
// and media DC 4. Keeping the wire bytes literal makes the decoder test independent from the
// helper below, which intentionally exercises the opposite (client) direction.
func TestAuthenticationMatchesOfficialTransportObfuscationFixture(t *testing.T) {
	wire, err := hex.DecodeString("000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f202122232425262728292a2b2c2d2e2f30313233343536377fb7a7ad45f7ade4")
	if err != nil {
		t.Fatal(err)
	}
	auth, err := readAuthentication(bytes.NewReader(wire))
	if err != nil {
		t.Fatal(err)
	}
	auth.applySecret([16]byte{0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15})
	xcrypto.NewAesCTRStream(auth.decodingKey[:], auth.decodingNonce[:]).XORKeyStream(auth.header[:], auth.header[:])
	if got := hex.EncodeToString(auth.decodingKey[:]); got != "58e43c97ffd7f8263296692cd28695bc3e29e03707aa98faa3be518061df6e67" {
		t.Fatalf("client key = %s", got)
	}
	if got := hex.EncodeToString(auth.encodingKey[:]); got != "f4effaeae14a397813a60bc9ff0da4a25a01c756fe62eeed037a5841d3cadcee" {
		t.Fatalf("server key = %s", got)
	}
	if got := auth.connectionType(); got != [4]byte{0xdd, 0xdd, 0xdd, 0xdd} {
		t.Fatalf("connection type = %x", got)
	}
	if got, ok := auth.dataCenterID(); !ok || got != -4 {
		t.Fatalf("data center = %d, %v", got, ok)
	}
}

func testClientHeader(secret [16]byte, connectionType [4]byte, dc int16) ([]byte, cipher.Stream, cipher.Stream) {
	plain := make([]byte, headerSize)
	for index := range plain {
		plain[index] = byte(index + 17)
	}
	copy(plain[56:60], connectionType[:])
	binary.LittleEndian.PutUint16(plain[60:62], uint16(dc))

	clientKey := append([]byte(nil), plain[8:40]...)
	clientNonce := append([]byte(nil), plain[40:56]...)
	reversed := append([]byte(nil), plain...)
	for left, right := 0, len(reversed)-1; left < right; left, right = left+1, right-1 {
		reversed[left], reversed[right] = reversed[right], reversed[left]
	}
	serverKey := append([]byte(nil), reversed[8:40]...)
	serverNonce := append([]byte(nil), reversed[40:56]...)
	clientKeyHash := sha256.Sum256(append(clientKey, secret[:]...))
	serverKeyHash := sha256.Sum256(append(serverKey, secret[:]...))

	clientEncrypt := xcrypto.NewAesCTRStream(clientKeyHash[:], clientNonce)
	encrypted := make([]byte, headerSize)
	clientEncrypt.XORKeyStream(encrypted, plain)
	wire := append([]byte(nil), plain...)
	copy(wire[56:], encrypted[56:])

	clientDecrypt := xcrypto.NewAesCTRStream(serverKeyHash[:], serverNonce)
	return wire, clientEncrypt, clientDecrypt
}

func TestPaddedIntermediateHeaderAndPayloadFollowTelegramCTRLayout(t *testing.T) {
	secret := [16]byte{0x99, 0x98, 0x97, 0x96, 0x95, 0x94, 0x93, 0x92, 0x91, 0x90, 0x89, 0x88, 0x87, 0x86, 0x85, 0x84}
	wire, clientEncrypt, clientDecrypt := testClientHeader(secret, [4]byte{0xdd, 0xdd, 0xdd, 0xdd}, -4)

	auth, err := readAuthentication(bytes.NewReader(wire))
	if err != nil {
		t.Fatal(err)
	}
	auth.applySecret(secret)
	xcrypto.NewAesCTRStream(auth.decodingKey[:], auth.decodingNonce[:]).XORKeyStream(auth.header[:], auth.header[:])
	if got := auth.connectionType(); got != [4]byte{0xdd, 0xdd, 0xdd, 0xdd} {
		t.Fatalf("connection type = %x", got)
	}
	if got, ok := auth.dataCenterID(); !ok || got != -4 {
		t.Fatalf("data center = %d, %v", got, ok)
	}

	request := []byte("padded intermediate request after the initialization payload")
	ciphertext := make([]byte, len(request))
	clientEncrypt.XORKeyStream(ciphertext, request)
	decoded := make([]byte, len(request))
	requestPayloadStream(auth.decodingKey, auth.decodingNonce).XORKeyStream(decoded, ciphertext)
	if !bytes.Equal(decoded, request) {
		t.Fatalf("request payload did not resume CTR after byte %d", headerSize)
	}

	response := []byte("telegram data-center response")
	ciphertext = make([]byte, len(response))
	xcrypto.NewAesCTRStream(auth.encodingKey[:], auth.encodingNonce[:]).XORKeyStream(ciphertext, response)
	decoded = make([]byte, len(response))
	clientDecrypt.XORKeyStream(decoded, ciphertext)
	if !bytes.Equal(decoded, response) {
		t.Fatalf("response payload did not resume CTR after byte %d", headerSize)
	}
}

func TestAuthenticateSelectsTheMatchingUserAndRejectsReplay(t *testing.T) {
	first := [16]byte{1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16}
	second := [16]byte{16, 15, 14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1}
	server := &Server{
		users: []*protocol.MemoryUser{
			{Email: "first@example.test", Account: &MemoryAccount{Secret: first}},
			{Email: "second@example.test", Account: &MemoryAccount{Secret: second}},
		},
		replaySeen: make(map[[32]byte]time.Time),
	}
	wire, _, _ := testClientHeader(second, [4]byte{0xee, 0xee, 0xee, 0xee}, 2)
	user, _, dc := server.authenticate(wire)
	if user == nil || user.Email != "second@example.test" || dc != 2 {
		t.Fatalf("authenticated user = %#v, dc = %d", user, dc)
	}
	now := time.Unix(1_700_000_000, 0)
	if server.replayed(wire, now) {
		t.Fatal("first header use was treated as replay")
	}
	if !server.replayed(wire, now.Add(time.Second)) {
		t.Fatal("duplicate header was not rejected")
	}
	if server.replayed(wire, now.Add(replayTTL+time.Second)) {
		t.Fatal("expired replay entry was not released")
	}
}

func TestDecodeSecretAcceptsUUIDAndHex(t *testing.T) {
	uuid, err := decodeSecret("00112233-4455-6677-8899-aabbccddeeff")
	if err != nil {
		t.Fatal(err)
	}
	hex, err := decodeSecret("00112233445566778899aabbccddeeff")
	if err != nil {
		t.Fatal(err)
	}
	if uuid != hex {
		t.Fatalf("UUID and hex decoded differently: %x != %x", uuid, hex)
	}
	if got := encodeSecret(hex); got != "00112233-4455-6677-8899-aabbccddeeff" {
		t.Fatalf("canonical secret = %q", got)
	}
}
