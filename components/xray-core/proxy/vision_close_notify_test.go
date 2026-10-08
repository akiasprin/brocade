package proxy

import (
	"bytes"
	"context"
	gotls "crypto/tls"
	"crypto/x509"
	gonet "net"
	"sync"
	"testing"
	"time"

	"github.com/xtls/xray-core/common/buf"
	"github.com/xtls/xray-core/common/protocol/tls/cert"
	"github.com/xtls/xray-core/transport/internet/stat"
	xraytls "github.com/xtls/xray-core/transport/internet/tls"
)

type closeWriteRecorder struct {
	gonet.Conn
	mu      sync.Mutex
	marked  bool
	written int
}

func (r *closeWriteRecorder) Write(payload []byte) (int, error) {
	n, err := r.Conn.Write(payload)
	r.mu.Lock()
	if r.marked {
		r.written += n
	}
	r.mu.Unlock()
	return n, err
}

func (r *closeWriteRecorder) mark() {
	r.mu.Lock()
	r.marked = true
	r.written = 0
	r.mu.Unlock()
}

func (r *closeWriteRecorder) bytesSinceMark() int {
	r.mu.Lock()
	defer r.mu.Unlock()
	return r.written
}

func handshakenVisionTLS(t *testing.T) (gonet.Conn, *closeWriteRecorder) {
	t.Helper()
	generated, _ := cert.MustGenerate(nil,
		cert.CommonName("vision.test"),
		cert.DNSNames("vision.test"),
		cert.KeyUsage(x509.KeyUsageDigitalSignature),
	)
	certificatePEM, keyPEM := generated.ToPEM()
	keyPair, err := gotls.X509KeyPair(certificatePEM, keyPEM)
	if err != nil {
		t.Fatal(err)
	}

	listener, err := gonet.Listen("tcp4", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	defer listener.Close()
	accepted := make(chan gonet.Conn, 1)
	go func() {
		conn, _ := listener.Accept()
		accepted <- conn
	}()
	rawClient, err := gonet.DialTimeout("tcp4", listener.Addr().String(), 5*time.Second)
	if err != nil {
		t.Fatal(err)
	}
	rawServer := <-accepted
	if rawServer == nil {
		t.Fatal("accept failed")
	}
	recorder := &closeWriteRecorder{Conn: rawServer}
	server := xraytls.Server(recorder, &gotls.Config{
		Certificates: []gotls.Certificate{keyPair},
		MinVersion:   gotls.VersionTLS13,
		MaxVersion:   gotls.VersionTLS13,
	})
	client := gotls.Client(rawClient, &gotls.Config{
		InsecureSkipVerify: true,
		MinVersion:         gotls.VersionTLS13,
		MaxVersion:         gotls.VersionTLS13,
	})
	deadline := time.Now().Add(5 * time.Second)
	_ = rawClient.SetDeadline(deadline)
	_ = rawServer.SetDeadline(deadline)
	handshake := make(chan error, 1)
	go func() { handshake <- server.(*xraytls.Conn).Handshake() }()
	if err := client.Handshake(); err != nil {
		t.Fatal(err)
	}
	if err := <-handshake; err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		_ = client.Close()
		_ = server.Close()
	})
	return &stat.CounterConnection{Connection: server}, recorder
}

func TestVisionOuterTLSCloseNotifyWithoutDirectCopy(t *testing.T) {
	conn, recorder := handshakenVisionTLS(t)
	recorder.mark()
	_ = conn.Close()
	if recorder.bytesSinceMark() == 0 {
		t.Fatal("ordinary TLS close did not write its close notification")
	}
}

func TestVisionWriterDirectCopySuppressesOuterCloseNotify(t *testing.T) {
	conn, recorder := handshakenVisionTLS(t)
	state := NewTrafficState([]byte("0123456789abcdef"))
	state.Inbound.IsPadding = false
	state.Inbound.DownlinkWriterDirectCopy = true
	writer := NewVisionWriter(buf.Discard, state, false, context.Background(), conn, nil, nil)
	if err := writer.WriteMultiBuffer(nil); err != nil {
		t.Fatal(err)
	}
	if state.Inbound.DownlinkWriterDirectCopy {
		t.Fatal("Vision writer did not enter direct-copy mode")
	}
	recorder.mark()
	_ = conn.Close()
	if written := recorder.bytesSinceMark(); written != 0 {
		t.Fatalf("outer TLS close wrote %d bytes after the Vision writer entered direct-copy mode", written)
	}
}

func TestVisionReaderDirectCopySuppressesOuterCloseNotify(t *testing.T) {
	conn, recorder := handshakenVisionTLS(t)
	uuid := []byte("0123456789abcdef")
	state := NewTrafficState(uuid)
	state.NumberOfPacketToFilter = 0
	writerUUID := append([]byte(nil), uuid...)
	payload := buf.FromBytes([]byte{0x17, 0x03, 0x03, 0x00, 0x01, 0x00})
	padded := XtlsPadding(payload, CommandPaddingDirect, &writerUUID, false, context.Background(), []uint32{1, 1, 1, 1})
	reader := NewVisionReader(&oneMultiBufferReader{buffer: buf.MultiBuffer{padded}}, state, true,
		context.Background(), conn, &bytes.Reader{}, &bytes.Buffer{}, nil)
	result, err := reader.ReadMultiBuffer()
	buf.ReleaseMulti(result)
	if err != nil {
		t.Fatal(err)
	}
	if !state.Inbound.UplinkReaderDirectCopy {
		t.Fatal("Vision reader did not enter direct-copy mode")
	}
	recorder.mark()
	_ = conn.Close()
	if written := recorder.bytesSinceMark(); written != 0 {
		t.Fatalf("outer TLS close wrote %d bytes after the Vision reader entered direct-copy mode", written)
	}
}

type oneMultiBufferReader struct {
	buffer buf.MultiBuffer
}

func (r *oneMultiBufferReader) ReadMultiBuffer() (buf.MultiBuffer, error) {
	result := r.buffer
	r.buffer = nil
	return result, nil
}
