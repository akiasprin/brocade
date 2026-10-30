package scenarios

import (
	"bytes"
	"crypto/md5"
	"crypto/sha256"
	"crypto/tls"
	"encoding/binary"
	"encoding/hex"
	"fmt"
	"io"
	stdnet "net"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/xtls/xray-core/common/protocol/tls/cert"
)

// Independent wire peer: no calls to proxy/anytls frame/session helpers.
type anytlsWireFrame struct {
	command byte
	stream  uint32
	body    []byte
}

func anytlsWireWrite(w io.Writer, command byte, stream uint32, body []byte) error {
	if len(body) > 65535 {
		return fmt.Errorf("fixture frame too large")
	}
	header := make([]byte, 7)
	header[0] = command
	binary.BigEndian.PutUint32(header[1:5], stream)
	binary.BigEndian.PutUint16(header[5:7], uint16(len(body)))
	wire := append(header, body...)
	for len(wire) > 0 {
		n, err := w.Write(wire)
		if err != nil {
			return err
		}
		if n == 0 {
			return io.ErrShortWrite
		}
		wire = wire[n:]
	}
	return nil
}
func anytlsWireRead(r io.Reader) (anytlsWireFrame, error) {
	var h [7]byte
	if _, err := io.ReadFull(r, h[:]); err != nil {
		return anytlsWireFrame{}, err
	}
	body := make([]byte, int(binary.BigEndian.Uint16(h[5:7])))
	_, err := io.ReadFull(r, body)
	return anytlsWireFrame{h[0], binary.BigEndian.Uint32(h[1:5]), body}, err
}
func anytlsWireAuth(w io.Writer, password string, padding int) error {
	sum := sha256.Sum256([]byte(password))
	wire := append(sum[:], byte(padding>>8), byte(padding))
	wire = append(wire, bytes.Repeat([]byte{0x52}, padding)...)
	_, err := w.Write(wire)
	return err
}
func anytlsWireTLS(t *testing.T, port int) *tls.Conn {
	t.Helper()
	c, err := tls.DialWithDialer(&stdnet.Dialer{Timeout: time.Second}, "tcp", fmt.Sprintf("127.0.0.1:%d", port), &tls.Config{InsecureSkipVerify: true, ServerName: "localhost", MinVersion: tls.VersionTLS13})
	if err != nil {
		t.Fatal(err)
	}
	_ = c.SetDeadline(time.Now().Add(3 * time.Second))
	t.Cleanup(func() { _ = c.Close() })
	return c
}
func anytlsWireSettings(t *testing.T, c stdnet.Conn, version, hash string) anytlsWireFrame {
	t.Helper()
	if err := anytlsWireAuth(c, frozenAlice, 17); err != nil {
		t.Fatal(err)
	}
	text := "client=independent-wire-peer"
	if version != "" {
		text += "\nv=" + version
	}
	if hash != "" {
		text += "\npadding-md5=" + hash
	}
	if err := anytlsWireWrite(c, 4, 0, []byte(text)); err != nil {
		t.Fatal(err)
	}
	f, err := anytlsWireRead(c)
	if err != nil {
		t.Fatal(err)
	}
	if f.command != 10 || f.stream != 0 || string(f.body) != "v=2" {
		t.Fatalf("unexpected server settings: %+v", f)
	}
	return f
}
func anytlsWireAddress(port int) []byte { return []byte{1, 127, 0, 0, 1, byte(port >> 8), byte(port)} }
func anytlsWireEcho(t *testing.T, c stdnet.Conn, sid uint32, payload []byte) {
	t.Helper()
	if err := anytlsWireWrite(c, 2, sid, payload); err != nil {
		t.Fatal(err)
	}
	got := []byte{}
	for i := 0; i < 8 && len(got) < len(payload); i++ {
		f, err := anytlsWireRead(c)
		if err != nil {
			t.Fatal(err)
		}
		if f.stream == sid && f.command == 2 {
			got = append(got, f.body...)
		} else if f.command != 7 && f.command != 3 {
			t.Fatalf("unexpected frame during echo: %+v", f)
		}
	}
	if !bytes.Equal(got, payload) {
		t.Fatalf("payload mismatch: got %d, want %d", len(got), len(payload))
	}
}
func anytlsWireOpen(t *testing.T, c stdnet.Conn, sid uint32, target int) {
	t.Helper()
	if err := anytlsWireWrite(c, 1, sid, nil); err != nil {
		t.Fatal(err)
	}
	if err := anytlsWireWrite(c, 2, sid, anytlsWireAddress(target)); err != nil {
		t.Fatal(err)
	}
	f, err := anytlsWireRead(c)
	if err != nil {
		t.Fatal(err)
	}
	if f.command != 7 || f.stream != sid || len(f.body) != 0 {
		t.Fatalf("open rejected: %+v", f)
	}
}

func TestBrocadeFrozenAnyTLSProtocolBoundaries(t *testing.T) {
	frozenBinary(t)
	entry := frozenPort(t)
	echo := frozenEcho(t)
	in := frozenIngress(t, "anytls", entry)
	scheme := "stop=2\n0=17-17\n1=64-64"
	in["settings"].(frozenObject)["paddingScheme"] = strings.Split(scheme, "\n")
	frozenStart(t, frozenObject{"inbounds": []any{in}, "outbounds": frozenFreedom()})
	for _, kind := range []string{"wrong-password", "short-auth", "truncated-auth-padding"} {
		t.Run(kind, func(t *testing.T) {
			c := anytlsWireTLS(t, entry)
			switch kind {
			case "wrong-password":
				if err := anytlsWireAuth(c, "wrong-fixture-password", 0); err != nil {
					t.Fatal(err)
				}
			case "short-auth":
				_, _ = c.Write([]byte("GET / HTTP/1.1\r\n"))
				_ = c.CloseWrite()
			case "truncated-auth-padding":
				sum := sha256.Sum256([]byte(frozenAlice))
				_, _ = c.Write(append(sum[:], 0, 64))
				_, _ = c.Write([]byte{1})
				_ = c.CloseWrite()
			}
			data, err := io.ReadAll(c)
			if err != nil {
				t.Fatal(err)
			}
			if !bytes.HasPrefix(data, []byte("HTTP/1.1 404")) || !bytes.Contains(data, []byte("404 page not found")) {
				t.Fatalf("missing bounded masquerade: %q", data)
			}
		})
	}
	for _, version := range []string{"", "1", "2", "255"} {
		t.Run("settings-version-"+version, func(t *testing.T) {
			c := anytlsWireTLS(t, entry)
			anytlsWireSettings(t, c, version, "00000000000000000000000000000000")
			update, err := anytlsWireRead(c)
			if err != nil {
				t.Fatal(err)
			}
			if update.command != 6 || update.stream != 0 || string(update.body) != scheme {
				t.Fatalf("padding update changed: %+v", update)
			}
			anytlsWireOpen(t, c, 1, echo)
			anytlsWireEcho(t, c, 1, []byte("no-downlink-padding"))
		})
	}
	t.Run("matching-padding-multiplex-fin-and-rejection", func(t *testing.T) {
		c := anytlsWireTLS(t, entry)
		sum := md5.Sum([]byte(scheme))
		anytlsWireSettings(t, c, "2", hex.EncodeToString(sum[:]))
		if err := anytlsWireWrite(c, 8, 0, nil); err != nil {
			t.Fatal(err)
		}
		f, err := anytlsWireRead(c)
		if err != nil || f.command != 9 {
			t.Fatalf("matching hash unexpectedly updated padding or heartbeat failed: %+v %v", f, err)
		}
		anytlsWireOpen(t, c, 1, echo)
		anytlsWireOpen(t, c, 3, echo)
		anytlsWireEcho(t, c, 1, []byte("first stream"))
		anytlsWireEcho(t, c, 3, bytes.Repeat([]byte{0x44}, 32768))
		// A malformed target rejects one stream without killing its sibling streams.
		if err := anytlsWireWrite(c, 1, 5, nil); err != nil {
			t.Fatal(err)
		}
		if err := anytlsWireWrite(c, 2, 5, []byte{0xff}); err != nil {
			t.Fatal(err)
		}
		f, err = anytlsWireRead(c)
		if err != nil || f.command != 7 || f.stream != 5 || len(f.body) == 0 {
			t.Fatalf("target error did not reject only its stream: %+v %v", f, err)
		}
		anytlsWireEcho(t, c, 3, []byte("sibling survives rejection"))
		if err := anytlsWireWrite(c, 3, 1, nil); err != nil {
			t.Fatal(err)
		}
		if err := anytlsWireWrite(c, 2, 1, []byte("late closed stream")); err != nil {
			t.Fatal(err)
		}
		anytlsWireOpen(t, c, 7, echo)
		anytlsWireEcho(t, c, 7, []byte("same carrier after FIN"))
	})
	t.Run("authenticated-frame-error-never-masquerades", func(t *testing.T) {
		c := anytlsWireTLS(t, entry)
		anytlsWireSettings(t, c, "2", "")
		if err := anytlsWireWrite(c, 1, 1, []byte("illegal SYN body")); err != nil {
			t.Fatal(err)
		}
		f, err := anytlsWireRead(c)
		if err != nil || f.command != 5 {
			t.Fatalf("expected protocol alert: %+v %v", f, err)
		}
		var b [1]byte
		if _, err := c.Read(b[:]); err == nil {
			t.Fatal("malformed protocol kept accepting data")
		}
	})
	t.Log("independent TLS wire peer verified auth, versions, padding negotiation, multiplex, stream-local rejection and FIN")
}

func TestBrocadeFrozenAnyTLSIdlePoolLifecycle(t *testing.T) {
	frozenBinary(t)
	certificate, _ := cert.MustGenerate(nil, cert.DNSNames("localhost"))
	pem, key := certificate.ToPEM()
	pair, err := tls.X509KeyPair(pem, key)
	if err != nil {
		t.Fatal(err)
	}
	listener, err := tls.Listen("tcp", "127.0.0.1:0", &tls.Config{Certificates: []tls.Certificate{pair}, MinVersion: tls.VersionTLS13})
	if err != nil {
		t.Fatal(err)
	}
	var accepted, live atomic.Int32
	var workers sync.WaitGroup
	var peers sync.Map
	fin := make(chan struct{}, 32)
	acceptedDone := make(chan struct{})
	t.Cleanup(func() {
		_ = listener.Close()
		<-acceptedDone
		peers.Range(func(k, v any) bool { _ = k.(stdnet.Conn).Close(); return true })
		workers.Wait()
	})
	go func() {
		defer close(acceptedDone)
		for {
			c, err := listener.Accept()
			if err != nil {
				return
			}
			accepted.Add(1)
			live.Add(1)
			peers.Store(c, true)
			workers.Add(1)
			go func() {
				defer workers.Done()
				defer c.Close()
				defer peers.Delete(c)
				defer live.Add(-1)
				_ = c.SetDeadline(time.Now().Add(20 * time.Second))
				var auth [34]byte
				if _, err := io.ReadFull(c, auth[:]); err != nil {
					return
				}
				sum := sha256.Sum256([]byte(frozenAlice))
				if !bytes.Equal(auth[:32], sum[:]) {
					return
				}
				padding := int(binary.BigEndian.Uint16(auth[32:]))
				if _, err := io.CopyN(io.Discard, c, int64(padding)); err != nil {
					return
				}
				targets := map[uint32]bool{}
				for {
					f, err := anytlsWireRead(c)
					if err != nil {
						return
					}
					switch f.command {
					case 4:
						if err := anytlsWireWrite(c, 10, 0, []byte("v=2")); err != nil {
							return
						}
					case 1:
						targets[f.stream] = false
					case 2:
						if !targets[f.stream] {
							if len(f.body) < 7 || f.body[0] != 1 {
								return
							}
							targets[f.stream] = true
							if err := anytlsWireWrite(c, 7, f.stream, nil); err != nil {
								return
							}
							f.body = f.body[7:]
						}
						if len(f.body) > 0 {
							if err := anytlsWireWrite(c, 2, f.stream, f.body); err != nil {
								return
							}
						}
					case 3:
						delete(targets, f.stream)
						fin <- struct{}{}
					case 8:
						if err := anytlsWireWrite(c, 9, 0, nil); err != nil {
							return
						}
					}
				}
			}()
		}
	}()
	local := frozenPort(t)
	frozenStart(t, frozenObject{"inbounds": []any{frozenDoko(local, 443)}, "outbounds": []any{frozenObject{"protocol": "anytls", "settings": frozenObject{"address": "127.0.0.1", "port": listener.Addr().(*stdnet.TCPAddr).Port, "password": frozenAlice, "idleSessionCheckInterval": 1, "idleSessionTimeout": 1, "minIdleSession": 1}, "streamSettings": frozenWire(t, "anytls", false)}}})
	streams := []stdnet.Conn{}
	for i := 0; i < 3; i++ {
		c := frozenDirect(t, local, "127.0.0.1")
		if err := frozenRoundtrip(c, []byte("simultaneous independent sessions")); err != nil {
			t.Fatal(err)
		}
		streams = append(streams, c)
	}
	if accepted.Load() != 3 {
		t.Fatalf("expected three active carriers, got %d", accepted.Load())
	}
	for _, c := range streams {
		_ = c.Close()
		select {
		case <-fin:
		case <-time.After(2 * time.Second):
			t.Fatal("client FIN was not sent")
		}
	}
	frozenEventually(t, 4*time.Second, func() bool { return live.Load() == 1 })
	before := accepted.Load()
	for i := 0; i < 3; i++ {
		c := frozenDirect(t, local, "127.0.0.1")
		if err := frozenRoundtrip(c, []byte("reuse retained idle session")); err != nil {
			t.Fatal(err)
		}
		_ = c.Close()
		select {
		case <-fin:
		case <-time.After(2 * time.Second):
			t.Fatal("reused stream did not FIN")
		}
	}
	if accepted.Load() != before {
		t.Fatalf("idle reuse opened a new carrier: before=%d after=%d", before, accepted.Load())
	}
	// Kill the idle transport; the next request must replace it and remain usable.
	peers.Range(func(k, v any) bool { _ = k.(stdnet.Conn).Close(); return true })
	frozenEventually(t, time.Second, func() bool { return live.Load() == 0 })
	c := frozenDirect(t, local, "127.0.0.1")
	if err := frozenRoundtrip(c, []byte("fresh after idle carrier loss")); err != nil {
		t.Fatal(err)
	}
	if accepted.Load() != before+1 {
		t.Fatalf("stale carrier was not replaced exactly once: %d", accepted.Load())
	}
	t.Logf("3 active carriers -> 1 idle reserve; three sequential streams reused it; lost idle carrier replaced once")
}

func TestBrocadeFrozenAnyTLSFINDeliversQueuedTail(t *testing.T) {
	frozenBinary(t)
	listener, err := stdnet.ListenTCP("tcp", &stdnet.TCPAddr{IP: stdnet.IPv4(127, 0, 0, 1)})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = listener.Close() })
	type receipt struct {
		data []byte
		err  error
	}
	received := make(chan receipt, 1)
	go func() {
		conn, err := listener.AcceptTCP()
		if err != nil {
			received <- receipt{err: err}
			return
		}
		defer conn.Close()
		_ = conn.SetReadBuffer(64 * 1024)
		_ = conn.SetReadDeadline(time.Now().Add(8 * time.Second))
		// The sender pipelines frames and FIN before the target starts draining.
		time.Sleep(100 * time.Millisecond)
		data, err := io.ReadAll(io.LimitReader(conn, 2*1024*1024))
		received <- receipt{data, err}
	}()
	entry := frozenPort(t)
	frozenStart(t, frozenObject{"inbounds": []any{frozenIngress(t, "anytls", entry)}, "outbounds": frozenFreedom()})
	conn := anytlsWireTLS(t, entry)
	_ = conn.SetDeadline(time.Now().Add(10 * time.Second))
	anytlsWireSettings(t, conn, "2", "")
	anytlsWireOpen(t, conn, 1, listener.Addr().(*stdnet.TCPAddr).Port)
	payload := make([]byte, 1024*1024+17)
	for i := range payload {
		payload[i] = byte(i*37 + 11)
	}
	for offset := 0; offset < len(payload); {
		end := min(offset+65535, len(payload))
		if err := anytlsWireWrite(conn, 2, 1, payload[offset:end]); err != nil {
			t.Fatal(err)
		}
		offset = end
	}
	if err := anytlsWireWrite(conn, 3, 1, nil); err != nil {
		t.Fatal(err)
	}
	select {
	case result := <-received:
		if result.err != nil || !bytes.Equal(result.data, payload) {
			t.Fatalf("FIN lost queued data: received=%d want=%d error=%v", len(result.data), len(payload), result.err)
		}
	case <-time.After(9 * time.Second):
		t.Fatal("target did not receive the complete tail and stream EOF")
	}
	// Stream completion must leave its authenticated carrier usable.
	echo := frozenEcho(t)
	anytlsWireOpen(t, conn, 3, echo)
	anytlsWireEcho(t, conn, 3, []byte("carrier survives completed FIN drain"))
	t.Logf("pipelined PSH + FIN delivered exact %d bytes and target EOF; same TLS carrier remains usable", len(payload))
}
