//go:build linux

package tls

import (
	"context"
	gotls "crypto/tls"
	"crypto/x509"
	stderrors "errors"
	"io"
	"net"
	"testing"
	"time"
	"unsafe"

	utls "github.com/refraction-networking/utls"
	"github.com/xtls/xray-core/common/platform"
	"github.com/xtls/xray-core/common/protocol/tls/cert"
	"golang.org/x/sys/unix"
)

func TestKernelTLSCryptoInfoABI(t *testing.T) {
	info128 := kernelTLSCryptoInfoAESGCM128{}
	if got := unsafe.Sizeof(info128); got != 40 {
		t.Fatalf("AES-128 crypto info size = %d, want 40", got)
	}
	if unsafe.Offsetof(info128.IV) != 4 || unsafe.Offsetof(info128.Key) != 12 ||
		unsafe.Offsetof(info128.Salt) != 28 || unsafe.Offsetof(info128.RecordSeq) != 32 {
		t.Fatal("AES-128 crypto info layout does not match linux/tls.h")
	}
	info256 := kernelTLSCryptoInfoAESGCM256{}
	if got := unsafe.Sizeof(info256); got != 56 {
		t.Fatalf("AES-256 crypto info size = %d, want 56", got)
	}
	if unsafe.Offsetof(info256.IV) != 4 || unsafe.Offsetof(info256.Key) != 12 ||
		unsafe.Offsetof(info256.Salt) != 44 || unsafe.Offsetof(info256.RecordSeq) != 48 {
		t.Fatal("AES-256 crypto info layout does not match linux/tls.h")
	}
}

func TestKernelTLSRecordTypeControlMessage(t *testing.T) {
	oob := make([]byte, unix.CmsgSpace(1))
	header := (*unix.Cmsghdr)(unsafe.Pointer(&oob[0]))
	header.Level = unix.SOL_TLS
	header.Type = kernelTLSGetRecordType
	header.SetLen(unix.CmsgLen(1))
	oob[unix.CmsgLen(0)] = tlsRecordTypeAlert
	recordType, err := kernelTLSRecordType(oob)
	if err != nil {
		t.Fatal(err)
	}
	if recordType != tlsRecordTypeAlert {
		t.Fatalf("record type = %d, want %d", recordType, tlsRecordTypeAlert)
	}
}

func TestKernelTLSLoopbackAESGCM(t *testing.T) {
	tests := []struct {
		name        string
		cipherSuite uint16
	}{
		{name: "AES-128-GCM", cipherSuite: utls.TLS_AES_128_GCM_SHA256},
		{name: "AES-256-GCM", cipherSuite: utls.TLS_AES_256_GCM_SHA384},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			testKernelTLSLoopback(t, test.cipherSuite)
		})
	}
}

func testKernelTLSLoopback(t *testing.T, cipherSuite uint16) {
	t.Setenv(platform.UseAnyTLSKernelTLS, "auto")
	certificate, _ := cert.MustGenerate(nil,
		cert.CommonName("localhost"),
		cert.DNSNames("localhost"),
		cert.KeyUsage(x509.KeyUsageDigitalSignature),
	)
	certificatePEM, keyPEM := certificate.ToPEM()
	keyPair, err := gotls.X509KeyPair(certificatePEM, keyPEM)
	if err != nil {
		t.Fatal(err)
	}

	listener, err := net.Listen("tcp4", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	defer listener.Close()

	type result struct {
		enabled bool
		err     error
	}
	serverResult := make(chan result, 1)
	go func() {
		rawConn, acceptErr := listener.Accept()
		if acceptErr != nil {
			serverResult <- result{err: acceptErr}
			return
		}
		serverConn := Server(rawConn, &gotls.Config{
			Certificates: []gotls.Certificate{keyPair},
			MinVersion:   gotls.VersionTLS13,
			MaxVersion:   gotls.VersionTLS13,
		}).(*Conn)
		enabled, enableErr := TryEnableKernelTLS(context.Background(), serverConn)
		if enableErr != nil {
			_ = serverConn.Close()
			serverResult <- result{err: enableErr}
			return
		}
		request := make([]byte, len("client payload"))
		if _, readErr := io.ReadFull(serverConn, request); readErr != nil {
			_ = serverConn.Close()
			serverResult <- result{enabled: enabled, err: readErr}
			return
		}
		if string(request) != "client payload" {
			_ = serverConn.Close()
			serverResult <- result{enabled: enabled, err: stderrors.New("server received an unexpected payload")}
			return
		}
		if _, writeErr := serverConn.Write([]byte("server payload")); writeErr != nil {
			_ = serverConn.Close()
			serverResult <- result{enabled: enabled, err: writeErr}
			return
		}
		serverResult <- result{enabled: enabled, err: serverConn.Close()}
	}()

	rawClient, err := net.DialTimeout("tcp4", listener.Addr().String(), 5*time.Second)
	if err != nil {
		t.Fatal(err)
	}
	client := utls.UClient(rawClient, &utls.Config{
		ServerName:         "localhost",
		InsecureSkipVerify: true,
	}, utls.HelloCustom)
	if err := client.ApplyPreset(kernelTLSTestClientHello(cipherSuite)); err != nil {
		t.Fatal(err)
	}
	if err := client.Handshake(); err != nil {
		server := <-serverResult
		t.Fatalf("client handshake: %v (server: %v)", err, server.err)
	}
	if got := client.ConnectionState().CipherSuite; got != cipherSuite {
		t.Fatalf("negotiated cipher suite = 0x%x, want 0x%x", got, cipherSuite)
	}
	if _, err := client.Write([]byte("client payload")); err != nil {
		t.Fatal(err)
	}
	response := make([]byte, len("server payload"))
	if _, err := io.ReadFull(client, response); err != nil {
		server := <-serverResult
		t.Fatalf("read response: %v (server: %v)", err, server.err)
	}
	if string(response) != "server payload" {
		t.Fatalf("client received %q, want %q", response, "server payload")
	}
	var final [1]byte
	if _, err := client.Read(final[:]); err != io.EOF {
		t.Fatalf("read after close = %v, want EOF", err)
	}
	if err := client.Close(); err != nil {
		t.Fatal(err)
	}
	server := <-serverResult
	if server.err != nil {
		t.Fatal(server.err)
	}
	if !server.enabled {
		t.Logf("kTLS unavailable for cipher suite 0x%x; verified transparent Go TLS fallback", cipherSuite)
	}
}

func kernelTLSTestClientHello(cipherSuite uint16) *utls.ClientHelloSpec {
	return &utls.ClientHelloSpec{
		TLSVersMin:         utls.VersionTLS13,
		TLSVersMax:         utls.VersionTLS13,
		CipherSuites:       []uint16{cipherSuite},
		CompressionMethods: []uint8{0},
		Extensions: []utls.TLSExtension{
			&utls.SNIExtension{},
			&utls.SupportedCurvesExtension{Curves: []utls.CurveID{utls.X25519}},
			&utls.SupportedPointsExtension{SupportedPoints: []byte{0}},
			&utls.SignatureAlgorithmsExtension{SupportedSignatureAlgorithms: []utls.SignatureScheme{
				utls.ECDSAWithP256AndSHA256,
				utls.PSSWithSHA256,
				utls.PSSWithSHA384,
			}},
			&utls.KeyShareExtension{KeyShares: []utls.KeyShare{{Group: utls.X25519}}},
			&utls.SupportedVersionsExtension{Versions: []uint16{utls.VersionTLS13}},
		},
	}
}
