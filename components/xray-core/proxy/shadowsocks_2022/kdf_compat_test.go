package shadowsocks_2022

import (
	"bytes"
	"crypto/sha256"
	"encoding/base64"
	"testing"
)

func TestLegacyOversizedPSKNormalization(t *testing.T) {
	for _, length := range []int{16, 32} {
		key := bytes.Repeat([]byte{0x42}, length+1)
		expected := sha256.Sum256(key)
		actual, err := ParseKey(base64.StdEncoding.EncodeToString(key), length)
		if err != nil || !bytes.Equal(actual, expected[:length]) {
			t.Fatalf("legacy normalization changed for key size %d: %v", length, err)
		}
		short := base64.StdEncoding.EncodeToString(key[:length-1])
		if _, err := ParseKey(short, length); err == nil {
			t.Fatal("short key was accepted")
		}
	}
}
