package anytls

import (
	"bytes"
	"context"
	"fmt"
	"net"
	"os"
	"runtime"
	"runtime/pprof"
	"testing"
	"time"

	"github.com/xtls/xray-core/common/buf"
	xnet "github.com/xtls/xray-core/common/net"
	"github.com/xtls/xray-core/transport"
)

func processFDCount() (int, bool) {
	entries, err := os.ReadDir("/proc/self/fd")
	if err != nil {
		return 0, false
	}
	return len(entries), true
}

func runLeakLifecycleRound(listener net.Listener, round int) error {
	acceptResult := make(chan struct {
		conn net.Conn
		err  error
	}, 1)
	go func() {
		conn, err := listener.Accept()
		acceptResult <- struct {
			conn net.Conn
			err  error
		}{conn: conn, err: err}
	}()
	clientConn, err := net.Dial("tcp", listener.Addr().String())
	if err != nil {
		return fmt.Errorf("dial lifecycle connection: %w", err)
	}
	accepted := <-acceptResult
	if accepted.err != nil {
		_ = clientConn.Close()
		return fmt.Errorf("accept lifecycle connection: %w", accepted.err)
	}

	client := newSessionForConn(clientConn, true)
	client.client = &Client{}
	server := newSessionForConn(accepted.conn, false)
	serverEndpoint := newTestLinkEndpoint()
	echoDone := startStressPipeEcho(serverEndpoint)
	server.dispatcher = &testDispatcher{dispatch: func(context.Context, xnet.Destination) (*transport.Link, error) {
		return serverEndpoint.link, nil
	}}
	clientReadDone := make(chan error, 1)
	serverReadDone := make(chan error, 1)
	go func() { clientReadDone <- client.readLoop(context.Background()) }()
	go func() { serverReadDone <- server.readLoop(context.Background()) }()

	clientEndpoint := newTestLinkEndpoint()
	ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
	stream, openErr := client.openStream(
		ctx,
		xnet.TCPDestination(xnet.DomainAddress("leak.example"), 443),
		clientEndpoint.link,
	)
	if openErr == nil {
		go stream.pumpUplink(client)
		payload := bytes.Repeat([]byte{byte(round)}, 1024+(round%17))
		if err := clientEndpoint.input.WriteMultiBuffer(buf.MultiBuffer{buf.FromBytes(payload)}); err != nil {
			openErr = fmt.Errorf("write lifecycle payload: %w", err)
		} else if received, err := readStressPayload(clientEndpoint.output, len(payload)); err != nil {
			openErr = fmt.Errorf("read lifecycle payload: %w", err)
		} else if !bytes.Equal(received, payload) {
			openErr = fmt.Errorf("lifecycle payload mismatch")
		}
	}
	cancel()
	client.close(openErr)
	server.close(openErr)
	clientEndpoint.closeInput()
	clientEndpoint.closeOutput()
	serverEndpoint.closeInput()
	serverEndpoint.closeOutput()

	shutdownCtx, shutdownCancel := context.WithTimeout(context.Background(), 3*time.Second)
	defer shutdownCancel()
	if err := waitStressChannel(shutdownCtx, "lifecycle echo", echoDone); err != nil && openErr == nil {
		openErr = err
	}
	for name, done := range map[string]<-chan error{"client read loop": clientReadDone, "server read loop": serverReadDone} {
		select {
		case <-done:
		case <-shutdownCtx.Done():
			if openErr == nil {
				openErr = fmt.Errorf("%s did not stop", name)
			}
		}
	}
	client.streamsMu.Lock()
	clientStreamCount := len(client.streams) + len(client.drainingStreams)
	client.streamsMu.Unlock()
	server.streamsMu.Lock()
	serverStreamCount := len(server.streams) + len(server.drainingStreams)
	server.streamsMu.Unlock()
	if openErr == nil && (clientStreamCount != 0 || serverStreamCount != 0 || client.activeStreams.Load() != 0) {
		openErr = fmt.Errorf(
			"lifecycle state did not converge: client_streams=%d server_streams=%d active=%d",
			clientStreamCount,
			serverStreamCount,
			client.activeStreams.Load(),
		)
	}
	return openErr
}

func TestSessionLifecycleDoesNotLeakGoroutinesOrFDs(t *testing.T) {
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	defer listener.Close()

	// Warm up the network poller and buffer pools before taking the baseline.
	if err := runLeakLifecycleRound(listener, 0); err != nil {
		t.Fatalf("warm-up lifecycle: %v", err)
	}
	runtime.GC()
	baselineGoroutines := runtime.NumGoroutine()
	baselineFDs, haveFDCount := processFDCount()

	const rounds = 64
	for round := 1; round <= rounds; round++ {
		if err := runLeakLifecycleRound(listener, round); err != nil {
			t.Fatalf("lifecycle round %d: %v", round, err)
		}
	}

	deadline := time.Now().Add(5 * time.Second)
	var goroutines int
	var fds int
	for {
		runtime.GC()
		goroutines = runtime.NumGoroutine()
		fds, _ = processFDCount()
		goroutinesConverged := goroutines <= baselineGoroutines+2
		fdsConverged := !haveFDCount || fds <= baselineFDs
		if goroutinesConverged && fdsConverged {
			return
		}
		if time.Now().After(deadline) {
			break
		}
		time.Sleep(25 * time.Millisecond)
	}

	var dump bytes.Buffer
	_ = pprof.Lookup("goroutine").WriteTo(&dump, 2)
	t.Fatalf(
		"AnyTLS lifecycle resources did not converge after %d rounds: goroutines=%d baseline=%d fds=%d baseline_fds=%d\n%s",
		rounds,
		goroutines,
		baselineGoroutines,
		fds,
		baselineFDs,
		dump.String(),
	)
}
