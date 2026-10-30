package hysteria

import (
	"bytes"
	"context"
	"crypto/ed25519"
	"crypto/rand"
	"crypto/tls"
	"crypto/x509"
	"fmt"
	"io"
	"math/big"
	"net/http"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/apernet/quic-go"
	"github.com/apernet/quic-go/http3"
	"github.com/xtls/xray-core/common/protocol"
	"github.com/xtls/xray-core/common/sourceaccess"
	"github.com/xtls/xray-core/common/sourceaccess/testutil"
	"github.com/xtls/xray-core/proxy/hysteria/account"
	"github.com/xtls/xray-core/transport/internet"
	"github.com/xtls/xray-core/transport/internet/stat"
)

func quotaQUICListener(t *testing.T, validator *account.Validator, staticAuth string, contexts ...context.Context) string {
	t.Helper()
	public, private, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		t.Fatal(err)
	}
	cert := &x509.Certificate{SerialNumber: big.NewInt(1), NotBefore: time.Now().Add(-time.Minute), NotAfter: time.Now().Add(time.Hour), KeyUsage: x509.KeyUsageDigitalSignature, ExtKeyUsage: []x509.ExtKeyUsage{x509.ExtKeyUsageServerAuth}}
	der, err := x509.CreateCertificate(rand.Reader, cert, cert, public, private)
	if err != nil {
		t.Fatal(err)
	}
	listener, err := quic.ListenAddr("127.0.0.1:0", &tls.Config{Certificates: []tls.Certificate{{Certificate: [][]byte{der}, PrivateKey: private}}, NextProtos: []string{http3.NextProtoH3}}, &quic.Config{EnableDatagrams: true})
	if err != nil {
		t.Fatal(err)
	}
	base := context.Background()
	if len(contexts) > 0 {
		base = contexts[0]
	}
	ctx, cancel := context.WithCancel(base)
	var workers sync.WaitGroup
	workers.Add(1)
	l := &Listener{ctx: ctx, validator: validator, config: &Config{Auth: staticAuth, UdpIdleTimeout: 5}, quicParams: &internet.QuicParams{Congestion: "reno"}, masqHandler: http.NotFoundHandler(), addConn: func(conn stat.Connection) {
		workers.Add(1)
		go func() { defer workers.Done(); defer conn.Close(); _, _ = io.Copy(conn, conn) }()
	}}
	go func() {
		defer workers.Done()
		for {
			conn, err := listener.Accept(ctx)
			if err != nil {
				return
			}
			workers.Add(1)
			go func() { defer workers.Done(); l.handleClient(conn) }()
		}
	}()
	t.Cleanup(func() {
		cancel()
		_ = listener.Close()
		done := make(chan struct{})
		go func() { workers.Wait(); close(done) }()
		select {
		case <-done:
		case <-time.After(3 * time.Second):
			t.Error("QUIC test handlers did not stop")
		}
	})
	return listener.Addr().String()
}

func quotaQUICAuthenticate(t *testing.T, address, auth string) (*quic.Conn, int) {
	t.Helper()
	opened := make(chan *quic.Conn, 1)
	transport := &http3.Transport{
		// Self-signed loopback-only fixture.
		TLSClientConfig: &tls.Config{InsecureSkipVerify: true}, QUICConfig: &quic.Config{EnableDatagrams: true},
		Dial: func(ctx context.Context, _ string, tlsConfig *tls.Config, config *quic.Config) (*quic.Conn, error) {
			conn, err := quic.DialAddr(ctx, address, tlsConfig, config)
			if err == nil {
				opened <- conn
			}
			return conn, err
		},
	}
	t.Cleanup(func() { _ = transport.Close() })
	ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
	defer cancel()
	request, err := http.NewRequestWithContext(ctx, http.MethodPost, "https://"+URLHost+URLPath, nil)
	if err != nil {
		t.Fatal(err)
	}
	request.Header.Set(RequestHeaderAuth, auth)
	response, err := transport.RoundTrip(request)
	if err != nil {
		t.Fatal(err)
	}
	_ = response.Body.Close()
	return <-opened, response.StatusCode
}

func quotaQUICStream(t *testing.T, conn *quic.Conn) *interConn {
	t.Helper()
	ctx, cancel := context.WithTimeout(context.Background(), time.Second)
	defer cancel()
	stream, err := conn.OpenStreamSync(ctx)
	if err != nil {
		t.Fatal(err)
	}
	return &interConn{stream: stream, local: conn.LocalAddr(), remote: conn.RemoteAddr(), client: true, lock: make(chan struct{}, 1)}
}

func quotaQUICExchange(conn *interConn) error {
	payload := []byte("quota isolation duplex payload")
	_ = conn.SetDeadline(time.Now().Add(time.Second))
	if _, err := conn.Write(payload); err != nil {
		return err
	}
	got := make([]byte, len(payload))
	if _, err := io.ReadFull(conn, got); err != nil {
		return err
	}
	if !bytes.Equal(got, payload) {
		return fmt.Errorf("TCP payload changed")
	}
	return nil
}

func quotaQUICDatagram(conn *quic.Conn) error {
	message := []byte{0, 0, 0, 1, 'q', 'u', 'o', 't', 'a'}
	if err := conn.SendDatagram(message); err != nil {
		return err
	}
	ctx, cancel := context.WithTimeout(context.Background(), time.Second)
	defer cancel()
	got, err := conn.ReceiveDatagram(ctx)
	if err != nil {
		return err
	}
	if !bytes.Equal(got, message) {
		return fmt.Errorf("UDP payload changed: %x", got)
	}
	return nil
}

func TestQuotaRevocationClosesOnlyUsersQUICWhileOtherUserKeepsTCPAndUDP(t *testing.T) {
	validator := account.NewValidator()
	alice := &protocol.MemoryUser{Email: "alice", Account: &account.MemoryAccount{Auth: "alice-test-auth"}}
	bob := &protocol.MemoryUser{Email: "bob", Account: &account.MemoryAccount{Auth: "bob-test-auth"}}
	for _, user := range []*protocol.MemoryUser{alice, bob} {
		if err := validator.Add(user); err != nil {
			t.Fatal(err)
		}
	}
	address := quotaQUICListener(t, validator, "")
	aliceQUIC, status := quotaQUICAuthenticate(t, address, "alice-test-auth")
	if status != StatusAuthOK {
		t.Fatal(status)
	}
	bobQUIC, status := quotaQUICAuthenticate(t, address, "bob-test-auth")
	if status != StatusAuthOK {
		t.Fatal(status)
	}
	aliceTCP, bobTCP := quotaQUICStream(t, aliceQUIC), quotaQUICStream(t, bobQUIC)
	for _, stream := range []*interConn{aliceTCP, bobTCP} {
		if err := quotaQUICExchange(stream); err != nil {
			t.Fatal(err)
		}
	}
	if err := quotaQUICDatagram(aliceQUIC); err != nil {
		t.Fatal(err)
	}
	var transfers atomic.Int32
	errors := make(chan error, 1)
	stop := make(chan struct{})
	done := make(chan struct{})
	go func() {
		defer close(done)
		ticker := time.NewTicker(5 * time.Millisecond)
		defer ticker.Stop()
		for {
			select {
			case <-stop:
				return
			case <-ticker.C:
				if err := quotaQUICExchange(bobTCP); err != nil {
					errors <- err
					return
				}
				if err := quotaQUICDatagram(bobQUIC); err != nil {
					errors <- err
					return
				}
				transfers.Add(1)
			}
		}
	}()
	defer func() { close(stop); <-done }()
	waitBob := func(min int32) {
		t.Helper()
		deadline := time.NewTimer(3 * time.Second)
		defer deadline.Stop()
		ticker := time.NewTicker(time.Millisecond)
		defer ticker.Stop()
		for transfers.Load() < min {
			select {
			case err := <-errors:
				t.Fatalf("other user's original TCP/UDP interrupted: %v", err)
			case <-deadline.C:
				t.Fatal("other user's original flows stopped")
			case <-ticker.C:
			}
		}
	}
	waitBob(3)
	before := transfers.Load()
	if err := validator.Del("alice"); err != nil {
		t.Fatal(err)
	}
	select {
	case <-aliceQUIC.Context().Done():
	case <-time.After(3 * time.Second):
		t.Fatal("revoked QUIC carrier stayed open")
	}
	if err := quotaQUICExchange(aliceTCP); err == nil {
		t.Fatal("revoked TCP stream still transfers")
	}
	if err := quotaQUICDatagram(aliceQUIC); err == nil {
		t.Fatal("revoked UDP session still transfers")
	}
	if _, status := quotaQUICAuthenticate(t, address, "alice-test-auth"); status == StatusAuthOK {
		t.Fatal("revoked user reauthenticated")
	}
	waitBob(before + 3)
	if bobQUIC.Context().Err() != nil {
		t.Fatal("other user's QUIC carrier was disconnected")
	}
	if err := validator.Add(alice); err != nil {
		t.Fatal(err)
	}
	restored, status := quotaQUICAuthenticate(t, address, "alice-test-auth")
	if status != StatusAuthOK {
		t.Fatal("restored user cannot authenticate")
	}
	if err := quotaQUICExchange(quotaQUICStream(t, restored)); err != nil {
		t.Fatal(err)
	}
	if err := quotaQUICDatagram(restored); err != nil {
		t.Fatal(err)
	}
	waitBob(before + 6)
}

func TestQuotaRemovalOfLastManagedUserDoesNotReopenStaticAuth(t *testing.T) {
	for _, managed := range []bool{false, true} {
		t.Run(fmt.Sprint(managed), func(t *testing.T) {
			validator := account.NewValidator()
			if managed {
				if err := validator.Add(&protocol.MemoryUser{Email: "alice", Account: &account.MemoryAccount{Auth: "user-auth"}}); err != nil {
					t.Fatal(err)
				}
				if err := validator.Del("alice"); err != nil {
					t.Fatal(err)
				}
			}
			address := quotaQUICListener(t, validator, "static-test-auth")
			_, status := quotaQUICAuthenticate(t, address, "static-test-auth")
			if (status == StatusAuthOK) == managed {
				t.Fatalf("managed=%v status=%d", managed, status)
			}
		})
	}
}

func TestSourceBanClosesWholeQUICWhileAnotherUserKeepsTCPAndUDP(t *testing.T) {
	ctx, update := testutil.Open(t)
	validator := account.NewValidator()
	for _, name := range []string{"alice", "bob"} {
		if err := validator.Add(&protocol.MemoryUser{Email: name + "@tenant#i1", Account: &account.MemoryAccount{Auth: name + "-auth"}}); err != nil {
			t.Fatal(err)
		}
	}
	address := quotaQUICListener(t, validator, "", ctx)
	alice, status := quotaQUICAuthenticate(t, address, "alice-auth")
	if status != StatusAuthOK {
		t.Fatal(status)
	}
	bob, status := quotaQUICAuthenticate(t, address, "bob-auth")
	if status != StatusAuthOK {
		t.Fatal(status)
	}
	a, b := quotaQUICStream(t, alice), quotaQUICStream(t, bob)
	if err := quotaQUICExchange(a); err != nil {
		t.Fatal(err)
	}
	if err := quotaQUICExchange(b); err != nil {
		t.Fatal(err)
	}
	update(sourceaccess.Policy{Revision: 2, Users: []sourceaccess.UserBlock{{Tenant: "tenant", User: "alice", IPs: []string{"127.0.0.1"}}}})
	select {
	case <-alice.Context().Done():
	case <-time.After(time.Second):
		t.Fatal("banned QUIC carrier survived")
	}
	if err := quotaQUICExchange(b); err != nil {
		t.Fatalf("other user's original TCP stream failed: %v", err)
	}
	if err := quotaQUICDatagram(bob); err != nil {
		t.Fatalf("other user's UDP failed: %v", err)
	}
	update(sourceaccess.Policy{Revision: 3})
	restored, status := quotaQUICAuthenticate(t, address, "alice-auth")
	if status != StatusAuthOK {
		t.Fatal(status)
	}
	if err := quotaQUICExchange(quotaQUICStream(t, restored)); err != nil {
		t.Fatal(err)
	}
}
