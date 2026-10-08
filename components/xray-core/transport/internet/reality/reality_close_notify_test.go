package reality

import (
	gotls "crypto/tls"
	"crypto/x509"
	gonet "net"
	"sync"
	"testing"
	"time"

	utls "github.com/refraction-networking/utls"
	realitytls "github.com/xtls/reality"
	"github.com/xtls/xray-core/common/protocol/tls/cert"
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

type closeNotifyConn interface {
	gonet.Conn
	Handshake() error
	SuppressCloseNotify()
}

func handshakenClient(t *testing.T, wrap func(gonet.Conn) closeNotifyConn) (closeNotifyConn, *closeWriteRecorder) {
	t.Helper()
	generated, _ := cert.MustGenerate(nil,
		cert.CommonName("reality-close.test"),
		cert.DNSNames("reality-close.test"),
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
	t.Cleanup(func() { _ = listener.Close() })
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
	deadline := time.Now().Add(5 * time.Second)
	_ = rawClient.SetDeadline(deadline)
	_ = rawServer.SetDeadline(deadline)
	recorder := &closeWriteRecorder{Conn: rawClient}
	client := wrap(recorder)
	server := gotls.Server(rawServer, &gotls.Config{
		Certificates: []gotls.Certificate{keyPair},
		MinVersion:   gotls.VersionTLS13,
		MaxVersion:   gotls.VersionTLS13,
	})
	handshake := make(chan error, 1)
	go func() { handshake <- server.Handshake() }()
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
	return client, recorder
}

func realityClient(raw gonet.Conn) closeNotifyConn {
	return &Conn{Conn: realitytls.Client(raw, &realitytls.Config{
		ServerName:         "reality-close.test",
		InsecureSkipVerify: true,
	})}
}

func utlsClient(raw gonet.Conn) closeNotifyConn {
	return &UConn{UConn: utls.UClient(raw, &utls.Config{
		ServerName:         "reality-close.test",
		InsecureSkipVerify: true,
		MinVersion:         utls.VersionTLS13,
		MaxVersion:         utls.VersionTLS13,
	}, utls.HelloGolang)}
}

func TestCloseNotifySuppression(t *testing.T) {
	for name, wrap := range map[string]func(gonet.Conn) closeNotifyConn{
		"reality": realityClient,
		"utls":    utlsClient,
	} {
		wrap := wrap
		t.Run(name, func(t *testing.T) {
			t.Run("normal", func(t *testing.T) {
				conn, recorder := handshakenClient(t, wrap)
				recorder.mark()
				_ = conn.Close()
				if recorder.bytesSinceMark() == 0 {
					t.Fatal("ordinary close did not write its close notification")
				}
			})
			t.Run("suppressed", func(t *testing.T) {
				conn, recorder := handshakenClient(t, wrap)
				conn.SuppressCloseNotify()
				recorder.mark()
				_ = conn.Close()
				if written := recorder.bytesSinceMark(); written != 0 {
					t.Fatalf("suppressed close wrote %d bytes", written)
				}
			})
		})
	}
}
