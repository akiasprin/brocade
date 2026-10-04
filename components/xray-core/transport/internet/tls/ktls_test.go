package tls

import (
	"bytes"
	gotls "crypto/tls"
	"net"
	"testing"

	"github.com/xtls/xray-core/common/platform"
)

func TestTryEnableKernelTLSModes(t *testing.T) {
	client, server := net.Pipe()
	defer client.Close()
	defer server.Close()

	t.Run("off", func(t *testing.T) {
		t.Setenv(platform.UseAnyTLSKernelTLS, "off")
		enabled, err := TryEnableKernelTLS(t.Context(), server)
		if err != nil || enabled {
			t.Fatalf("TryEnableKernelTLS() = (%v, %v), want (false, nil)", enabled, err)
		}
	})
	t.Run("auto non-TLS", func(t *testing.T) {
		t.Setenv(platform.UseAnyTLSKernelTLS, "auto")
		enabled, err := TryEnableKernelTLS(t.Context(), server)
		if err != nil || enabled {
			t.Fatalf("TryEnableKernelTLS() = (%v, %v), want (false, nil)", enabled, err)
		}
	})
	t.Run("required non-TLS", func(t *testing.T) {
		t.Setenv(platform.UseAnyTLSKernelTLS, "required")
		if _, err := TryEnableKernelTLS(t.Context(), server); err == nil {
			t.Fatal("required mode accepted a non-TLS connection")
		}
	})
	t.Run("invalid", func(t *testing.T) {
		t.Setenv(platform.UseAnyTLSKernelTLS, "sometimes")
		if _, err := TryEnableKernelTLS(t.Context(), server); err == nil {
			t.Fatal("invalid mode was accepted")
		}
	})
}

func TestDeriveKernelTLSKeyMaterialSupportsAESGCM(t *testing.T) {
	tests := []struct {
		name        string
		cipherSuite uint16
		secretLen   int
		keyLen      int
	}{
		{name: "AES-128-GCM", cipherSuite: gotls.TLS_AES_128_GCM_SHA256, secretLen: 32, keyLen: 16},
		{name: "AES-256-GCM", cipherSuite: gotls.TLS_AES_256_GCM_SHA384, secretLen: 48, keyLen: 32},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			txSecret := bytes.Repeat([]byte{0x11}, test.secretLen)
			rxSecret := bytes.Repeat([]byte{0x22}, test.secretLen)
			material, err := deriveKernelTLSKeyMaterial(test.cipherSuite, txSecret, rxSecret)
			if err != nil {
				t.Fatal(err)
			}
			defer material.clear()
			if len(material.txKey) != test.keyLen || len(material.rxKey) != test.keyLen {
				t.Fatalf("key lengths = (%d, %d), want (%d, %d)", len(material.txKey), len(material.rxKey), test.keyLen, test.keyLen)
			}
			if len(material.txIV) != 12 || len(material.rxIV) != 12 {
				t.Fatalf("IV lengths = (%d, %d), want (12, 12)", len(material.txIV), len(material.rxIV))
			}
			if bytes.Equal(material.txKey, material.rxKey) || bytes.Equal(material.txIV, material.rxIV) {
				t.Fatal("different traffic secrets derived identical key material")
			}
		})
	}
}

func TestDeriveKernelTLSKeyMaterialRejectsChaCha(t *testing.T) {
	if _, err := deriveKernelTLSKeyMaterial(gotls.TLS_CHACHA20_POLY1305_SHA256, make([]byte, 32), make([]byte, 32)); err == nil {
		t.Fatal("ChaCha20-Poly1305 unexpectedly accepted")
	}
}

func TestTrafficSecretCapture(t *testing.T) {
	capture := newTrafficSecretCapture()
	input := "CLIENT_HANDSHAKE_TRAFFIC_SECRET 00 aa\n" +
		"CLIENT_TRAFFIC_SECRET_0 00 " + string(bytes.Repeat([]byte("11"), 32)) + "\n" +
		"SERVER_TRAFFIC_SECRET_0 00 " + string(bytes.Repeat([]byte("22"), 32)) + "\n"
	for _, chunk := range []string{input[:17], input[17:71], input[71:]} {
		if _, err := capture.Write([]byte(chunk)); err != nil {
			t.Fatal(err)
		}
	}
	client, server, ok := capture.takeTrafficSecrets()
	if !ok {
		t.Fatal("traffic secrets were not captured")
	}
	defer clear(client)
	defer clear(server)
	if len(client) != 32 || len(server) != 32 {
		t.Fatalf("secret lengths = (%d, %d), want (32, 32)", len(client), len(server))
	}
}
