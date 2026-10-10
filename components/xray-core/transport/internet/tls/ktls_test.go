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

func TestTryEnableKernelTLSReportsNonFatalFallbackReason(t *testing.T) {
	client, server := net.Pipe()
	defer client.Close()
	defer server.Close()

	t.Run("disabled", func(t *testing.T) {
		t.Setenv(platform.UseAnyTLSKernelTLS, "off")
		enabled, reason, err := TryEnableKernelTLSWithReason(t.Context(), server)
		if err != nil || enabled || reason != KernelTLSFallbackDisabled {
			t.Fatalf("TryEnableKernelTLSWithReason() = (%v, %q, %v), want (false, %q, nil)", enabled, reason, err, KernelTLSFallbackDisabled)
		}
	})
	t.Run("unsupported connection", func(t *testing.T) {
		t.Setenv(platform.UseAnyTLSKernelTLS, "auto")
		enabled, reason, err := TryEnableKernelTLSWithReason(t.Context(), server)
		if err != nil || enabled || reason != KernelTLSFallbackUnsupportedConnection {
			t.Fatalf("TryEnableKernelTLSWithReason() = (%v, %q, %v), want (false, %q, nil)", enabled, reason, err, KernelTLSFallbackUnsupportedConnection)
		}
	})
}

func TestDeriveKernelTLSTXKeyMaterialSupportsAESGCM(t *testing.T) {
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
			material, err := deriveKernelTLSTXKeyMaterial(test.cipherSuite, txSecret)
			if err != nil {
				t.Fatal(err)
			}
			defer material.clear()
			if len(material.key) != test.keyLen {
				t.Fatalf("key length = %d, want %d", len(material.key), test.keyLen)
			}
			if len(material.iv) != 12 {
				t.Fatalf("IV length = %d, want 12", len(material.iv))
			}
		})
	}
}

func TestDeriveKernelTLSTXKeyMaterialRejectsChaCha(t *testing.T) {
	if _, err := deriveKernelTLSTXKeyMaterial(gotls.TLS_CHACHA20_POLY1305_SHA256, make([]byte, 32)); err == nil {
		t.Fatal("ChaCha20-Poly1305 unexpectedly accepted")
	}
}

func TestServerTrafficSecretCaptureIgnoresClientSecret(t *testing.T) {
	capture := newServerTrafficSecretCapture()
	input := "CLIENT_HANDSHAKE_TRAFFIC_SECRET 00 aa\n" +
		"CLIENT_TRAFFIC_SECRET_0 00 " + string(bytes.Repeat([]byte("11"), 32)) + "\n" +
		"SERVER_TRAFFIC_SECRET_0 00 " + string(bytes.Repeat([]byte("22"), 32)) + "\n"
	for _, chunk := range []string{input[:17], input[17:71], input[71:]} {
		if _, err := capture.Write([]byte(chunk)); err != nil {
			t.Fatal(err)
		}
	}
	server, ok := capture.takeServerTrafficSecret()
	if !ok {
		t.Fatal("server traffic secret was not captured")
	}
	defer clear(server)
	if len(server) != 32 {
		t.Fatalf("server secret length = %d, want 32", len(server))
	}
	if !bytes.Equal(server, bytes.Repeat([]byte{0x22}, 32)) {
		t.Fatal("captured client traffic secret instead of server traffic secret")
	}
}
