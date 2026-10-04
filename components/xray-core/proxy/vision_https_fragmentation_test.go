package proxy

import (
	"bufio"
	"bytes"
	"context"
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/rand"
	"crypto/tls"
	"crypto/x509"
	"crypto/x509/pkix"
	"io"
	"math/big"
	mathrand "math/rand"
	stdnet "net"
	"sync"
	"testing"
	"time"

	"github.com/xtls/xray-core/common/buf"
	"github.com/xtls/xray-core/common/session"
)

// TestVisionWriterRealTLS13RandomFragmentation feeds records captured from an
// actual TLS 1.3 HTTP exchange through separate WriteMultiBuffer calls. This is
// intentionally different from splitting one MultiBuffer: production TCP/TLS
// readers are free to end any read in the middle of a record.
func TestVisionWriterRealTLS13RandomFragmentation(t *testing.T) {
	clientToServer, serverToClient := captureTLS13HTTPExchange(t)
	clientHelloSize, ok := tlsRecordSize(clientToServer)
	if !ok || clientHelloSize > len(clientToServer) {
		t.Fatal("captured client flight does not start with a complete TLS record")
	}
	clientHello := clientToServer[:clientHelloSize]

	seeds := 128
	if testing.Short() {
		seeds = 16
	}
	fragmentLimits := []int{1, 2, 3, 5, 7, 16, 31, 257, 1500, 8192}
	for seed := 0; seed < seeds; seed++ {
		seed := seed
		t.Run("seed-"+big.NewInt(int64(seed)).String(), func(t *testing.T) {
			userID := bytes.Repeat([]byte{byte(seed + 1)}, 16)
			trafficState := NewTrafficState(userID)
			metrics := new(spliceTestMetrics)
			inbound := &session.Inbound{SpliceMetrics: metrics}
			inbound.CanSpliceCopy.Store(session.SpliceCopyWaiting)
			ctx := session.ContextWithInbound(context.Background(), inbound)

			uplinkOuter := new(bytes.Buffer)
			uplinkRaw := newMemoryConn()
			uplink := NewVisionWriter(buf.NewWriter(uplinkOuter), trafficState, true, ctx, uplinkRaw, nil, []uint32{900, 1, 900, 1})
			feedVisionFragments(t, uplink, clientHello, int64(seed)*2+1, fragmentLimits[seed%len(fragmentLimits)])

			downlinkOuter := new(bytes.Buffer)
			downlinkRaw := newMemoryConn()
			downlink := NewVisionWriter(buf.NewWriter(downlinkOuter), trafficState, false, ctx, downlinkRaw, nil, []uint32{900, 1, 900, 1})
			feedVisionFragments(t, downlink, serverToClient, int64(seed)*2+2, fragmentLimits[(seed+3)%len(fragmentLimits)])

			if got := inbound.CanSpliceCopy.Load(); got != session.SpliceCopyDirect {
				t.Fatalf("seed %d: splice state = %d, want direct", seed, got)
			}
			if got := metrics.direct.Load(); got != 1 {
				t.Fatalf("seed %d: direct transitions = %d, want 1", seed, got)
			}
			if !trafficState.tlsSnapshot().enableXtls {
				t.Fatalf("seed %d: real TLS 1.3 ServerHello was not recognized", seed)
			}
			rawBytes := len(downlinkRaw.Bytes())
			if rawBytes <= len(serverToClient)/2 {
				t.Fatalf("seed %d: only %d/%d bytes reached direct mode", seed, rawBytes, len(serverToClient))
			}
			if got := metrics.directB.Load(); got != int64(rawBytes) {
				t.Fatalf("seed %d: observed direct bytes = %d, want %d", seed, got, rawBytes)
			}

			decoded := decodeVisionPadding(t, userID, downlinkOuter.Bytes())
			decoded = append(decoded, downlinkRaw.Bytes()...)
			if !bytes.Equal(decoded, serverToClient) {
				t.Fatalf("seed %d: downlink changed across randomized fragmentation: got %d bytes, want %d", seed, len(decoded), len(serverToClient))
			}
		})
	}
}

func TestVisionWriterDoesNotDelayShortNonTLSWrite(t *testing.T) {
	userID := bytes.Repeat([]byte{0x42}, 16)
	state := NewTrafficState(userID)
	outer := new(bytes.Buffer)
	writer := NewVisionWriter(buf.NewWriter(outer), state, true, context.Background(), newMemoryConn(), nil, []uint32{900, 1, 900, 1})
	payload := []byte{0x16}
	if err := writer.WriteMultiBuffer(buf.MultiBuffer{buf.FromBytes(payload)}); err != nil {
		t.Fatal(err)
	}
	if outer.Len() == 0 {
		t.Fatal("one-byte write was delayed while waiting for a TLS header")
	}
	if got := decodeVisionPadding(t, userID, outer.Bytes()); !bytes.Equal(got, payload) {
		t.Fatalf("decoded short write = %x, want %x", got, payload)
	}
}

func feedVisionFragments(t *testing.T, writer *VisionWriter, payload []byte, seed int64, maxCallSize int) {
	t.Helper()
	rng := mathrand.New(mathrand.NewSource(seed))
	for offset := 0; offset < len(payload); {
		callSize := 1 + rng.Intn(maxCallSize)
		if callSize > len(payload)-offset {
			callSize = len(payload) - offset
		}
		call := append([]byte(nil), payload[offset:offset+callSize]...)
		offset += callSize

		var mb buf.MultiBuffer
		for len(call) > 0 {
			bufferSize := 1 + rng.Intn(len(call))
			mb = append(mb, buf.FromBytes(call[:bufferSize]))
			call = call[bufferSize:]
		}
		if err := writer.WriteMultiBuffer(mb); err != nil {
			t.Fatalf("seed %d: fragmented write failed: %v", seed, err)
		}
	}
}

func decodeVisionPadding(t *testing.T, userID, encoded []byte) []byte {
	t.Helper()
	state := NewTrafficState(userID)
	b := buf.FromBytes(append([]byte(nil), encoded...))
	decoded := XtlsUnpadding(b, state, false, context.Background())
	defer decoded.Release()
	return append([]byte(nil), decoded.Bytes()...)
}

type captureConn struct {
	stdnet.Conn
	mu      sync.Mutex
	written bytes.Buffer
}

func (c *captureConn) Write(p []byte) (int, error) {
	c.mu.Lock()
	_, _ = c.written.Write(p)
	c.mu.Unlock()
	return c.Conn.Write(p)
}

func (c *captureConn) Bytes() []byte {
	c.mu.Lock()
	defer c.mu.Unlock()
	return append([]byte(nil), c.written.Bytes()...)
}

func captureTLS13HTTPExchange(t *testing.T) ([]byte, []byte) {
	t.Helper()
	certificate := testTLSCertificate(t)
	clientSide, serverSide := stdnet.Pipe()
	clientCapture := &captureConn{Conn: clientSide}
	serverCapture := &captureConn{Conn: serverSide}
	deadline := time.Now().Add(10 * time.Second)
	_ = clientCapture.SetDeadline(deadline)
	_ = serverCapture.SetDeadline(deadline)

	serverErr := make(chan error, 1)
	go func() {
		conn := tls.Server(serverCapture, &tls.Config{
			Certificates: []tls.Certificate{certificate},
			MinVersion:   tls.VersionTLS13,
			MaxVersion:   tls.VersionTLS13,
		})
		defer conn.Close()
		reader := bufio.NewReader(conn)
		for {
			line, err := reader.ReadString('\n')
			if err != nil {
				serverErr <- err
				return
			}
			if line == "\r\n" {
				break
			}
		}
		body := bytes.Repeat([]byte("real-tls13-http-payload-"), 4096)
		if _, err := conn.Write([]byte("HTTP/1.1 200 OK\r\nContent-Length: " + big.NewInt(int64(len(body))).String() + "\r\nConnection: close\r\n\r\n")); err != nil {
			serverErr <- err
			return
		}
		for len(body) > 0 {
			size := 13 * 1024
			if size > len(body) {
				size = len(body)
			}
			if _, err := conn.Write(body[:size]); err != nil {
				serverErr <- err
				return
			}
			body = body[size:]
		}
		serverErr <- nil
	}()

	client := tls.Client(clientCapture, &tls.Config{
		InsecureSkipVerify: true, // The certificate is generated only for this in-memory test.
		MinVersion:         tls.VersionTLS13,
		MaxVersion:         tls.VersionTLS13,
		ServerName:         "localhost",
	})
	if _, err := io.WriteString(client, "GET /large HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"); err != nil {
		t.Fatal(err)
	}
	if _, err := io.ReadAll(client); err != nil {
		t.Fatal(err)
	}
	_ = client.Close()
	if err := <-serverErr; err != nil {
		t.Fatal(err)
	}
	return clientCapture.Bytes(), serverCapture.Bytes()
}

func testTLSCertificate(t *testing.T) tls.Certificate {
	t.Helper()
	key, err := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	if err != nil {
		t.Fatal(err)
	}
	now := time.Now()
	template := &x509.Certificate{
		SerialNumber: big.NewInt(1),
		Subject:      pkix.Name{CommonName: "localhost"},
		DNSNames:     []string{"localhost"},
		NotBefore:    now.Add(-time.Minute),
		NotAfter:     now.Add(time.Hour),
		KeyUsage:     x509.KeyUsageDigitalSignature,
		ExtKeyUsage:  []x509.ExtKeyUsage{x509.ExtKeyUsageServerAuth},
	}
	der, err := x509.CreateCertificate(rand.Reader, template, template, &key.PublicKey, key)
	if err != nil {
		t.Fatal(err)
	}
	return tls.Certificate{Certificate: [][]byte{der}, PrivateKey: key}
}

type memoryConn struct {
	mu sync.Mutex
	b  bytes.Buffer
}

func newMemoryConn() *memoryConn { return new(memoryConn) }

func (c *memoryConn) Read([]byte) (int, error) { return 0, io.EOF }

func (c *memoryConn) Write(p []byte) (int, error) {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.b.Write(p)
}

func (c *memoryConn) Close() error                     { return nil }
func (c *memoryConn) LocalAddr() stdnet.Addr           { return memoryAddr("local") }
func (c *memoryConn) RemoteAddr() stdnet.Addr          { return memoryAddr("remote") }
func (c *memoryConn) SetDeadline(time.Time) error      { return nil }
func (c *memoryConn) SetReadDeadline(time.Time) error  { return nil }
func (c *memoryConn) SetWriteDeadline(time.Time) error { return nil }
func (c *memoryConn) Bytes() []byte {
	c.mu.Lock()
	defer c.mu.Unlock()
	return append([]byte(nil), c.b.Bytes()...)
}

type memoryAddr string

func (a memoryAddr) Network() string { return "memory" }
func (a memoryAddr) String() string  { return string(a) }
