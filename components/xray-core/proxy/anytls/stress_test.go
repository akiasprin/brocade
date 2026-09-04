package anytls

import (
	"bytes"
	"context"
	"fmt"
	"io"
	"net"
	"sync"
	"testing"
	"time"

	"github.com/xtls/xray-core/common/buf"
	xnet "github.com/xtls/xray-core/common/net"
	"github.com/xtls/xray-core/transport"
)

func startStressPipeEcho(endpoint *testLinkEndpoint) <-chan struct{} {
	done := make(chan struct{})
	go func() {
		defer close(done)
		for {
			mb, err := endpoint.output.ReadMultiBuffer()
			if err != nil {
				buf.ReleaseMulti(mb)
				return
			}
			if err := endpoint.input.WriteMultiBuffer(mb); err != nil {
				return
			}
		}
	}()
	return done
}

func readStressPayload(reader buf.Reader, length int) ([]byte, error) {
	result := make([]byte, 0, length)
	for len(result) < length {
		mb, err := reader.ReadMultiBuffer()
		if err != nil {
			buf.ReleaseMulti(mb)
			return nil, err
		}
		chunk := make([]byte, mb.Len())
		mb.Copy(chunk)
		buf.ReleaseMulti(mb)
		result = append(result, chunk...)
		if len(result) > length {
			return nil, fmt.Errorf("received %d bytes, expected %d", len(result), length)
		}
	}
	return result, nil
}

func waitStressChannel(ctx context.Context, name string, done <-chan struct{}) error {
	select {
	case <-done:
		return nil
	case <-ctx.Done():
		return fmt.Errorf("%s did not stop: %w", name, ctx.Err())
	}
}

func newStressSessionPair(t *testing.T) (*session, *session) {
	t.Helper()
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
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
		_ = listener.Close()
		t.Fatal(err)
	}
	accepted := <-acceptResult
	_ = listener.Close()
	if accepted.err != nil {
		_ = clientConn.Close()
		t.Fatal(accepted.err)
	}
	client := newSessionForConn(clientConn, true)
	client.client = &Client{}
	return client, newSessionForConn(accepted.conn, false)
}

func exerciseStressStream(ctx context.Context, client *session, worker, round int) error {
	endpoint := newTestLinkEndpoint()
	defer endpoint.closeInput()
	defer endpoint.closeOutput()

	target := xnet.TCPDestination(
		xnet.DomainAddress(fmt.Sprintf("worker-%d-%d.example", worker, round)),
		443,
	)
	stream, err := client.openStream(ctx, target, endpoint.link)
	if err != nil {
		return fmt.Errorf("open stream: %w", err)
	}
	go stream.pumpUplink(client)

	sizes := [...]int{
		1,
		257,
		int(buf.Size) + 17,
		maxFramePayload - 1,
		maxFramePayload,
		maxFramePayload + 1,
		2*maxFramePayload + 31,
	}
	payload := make([]byte, sizes[(worker+round)%len(sizes)])
	for index := range payload {
		payload[index] = byte((index*31 + worker*17 + round*13) % 251)
	}
	if err := endpoint.input.WriteMultiBuffer(buf.MultiBuffer{buf.FromBytes(payload)}); err != nil {
		return fmt.Errorf("write stream payload: %w", err)
	}

	// Leave a deterministic fraction of streams without a downlink consumer. The
	// session still has to process their FIN while other streams make progress.
	if (worker+round)%5 != 0 {
		received, err := readStressPayload(endpoint.output, len(payload))
		if err != nil {
			return fmt.Errorf("read stream payload: %w", err)
		}
		if !bytes.Equal(received, payload) {
			return fmt.Errorf("payload mismatch: got %d bytes, want %d", len(received), len(payload))
		}
	}

	endpoint.closeInput()
	select {
	case <-stream.done:
		return nil
	case <-ctx.Done():
		return fmt.Errorf("stream did not close: %w", ctx.Err())
	}
}

func TestSessionStressConcurrentStreams(t *testing.T) {
	client, server := newStressSessionPair(t)

	var resourcesMu sync.Mutex
	var serverEndpoints []*testLinkEndpoint
	var echoDone []<-chan struct{}
	server.dispatcher = &testDispatcher{dispatch: func(context.Context, xnet.Destination) (*transport.Link, error) {
		endpoint := newTestLinkEndpoint()
		done := startStressPipeEcho(endpoint)
		resourcesMu.Lock()
		serverEndpoints = append(serverEndpoints, endpoint)
		echoDone = append(echoDone, done)
		resourcesMu.Unlock()
		return endpoint.link, nil
	}}

	clientReadDone := make(chan error, 1)
	serverReadDone := make(chan error, 1)
	go func() { clientReadDone <- client.readLoop(context.Background()) }()
	go func() { serverReadDone <- server.readLoop(context.Background()) }()

	ctx, cancel := context.WithTimeout(context.Background(), 20*time.Second)
	defer cancel()
	const workers = 32
	const rounds = 8
	start := make(chan struct{})
	workerErrors := make(chan error, workers)
	var workersDone sync.WaitGroup
	workersDone.Add(workers)
	for worker := range workers {
		go func() {
			defer workersDone.Done()
			<-start
			for round := range rounds {
				if err := exerciseStressStream(ctx, client, worker, round); err != nil {
					workerErrors <- fmt.Errorf("worker %d round %d: %w", worker, round, err)
					return
				}
			}
		}()
	}
	close(start)
	waitDone := make(chan struct{})
	go func() {
		workersDone.Wait()
		close(waitDone)
	}()
	select {
	case <-waitDone:
	case <-ctx.Done():
		client.close(ctx.Err())
		server.close(ctx.Err())
		<-waitDone
	}
	close(workerErrors)
	for err := range workerErrors {
		t.Error(err)
	}

	client.close(nil)
	server.close(nil)
	resourcesMu.Lock()
	endpoints := append([]*testLinkEndpoint(nil), serverEndpoints...)
	echoes := append([]<-chan struct{}(nil), echoDone...)
	resourcesMu.Unlock()
	for _, endpoint := range endpoints {
		endpoint.closeInput()
		endpoint.closeOutput()
	}
	shutdownCtx, shutdownCancel := context.WithTimeout(context.Background(), 3*time.Second)
	defer shutdownCancel()
	for index, done := range echoes {
		if err := waitStressChannel(shutdownCtx, fmt.Sprintf("echo worker %d", index), done); err != nil {
			t.Error(err)
		}
	}
	for name, done := range map[string]<-chan error{"client read loop": clientReadDone, "server read loop": serverReadDone} {
		select {
		case <-done:
		case <-shutdownCtx.Done():
			t.Errorf("%s did not stop", name)
		}
	}

	client.streamsMu.Lock()
	clientStreams := len(client.streams) + len(client.drainingStreams)
	client.streamsMu.Unlock()
	server.streamsMu.Lock()
	serverStreams := len(server.streams) + len(server.drainingStreams)
	server.streamsMu.Unlock()
	if clientStreams != 0 || serverStreams != 0 || client.activeStreams.Load() != 0 {
		t.Fatalf(
			"session state did not converge: client_streams=%d server_streams=%d active=%d",
			clientStreams,
			serverStreams,
			client.activeStreams.Load(),
		)
	}
}

func TestSessionStressConcurrentClose(t *testing.T) {
	client, server := newStressSessionPair(t)

	var resourcesMu sync.Mutex
	var endpoints []*testLinkEndpoint
	var echoes []<-chan struct{}
	server.dispatcher = &testDispatcher{dispatch: func(context.Context, xnet.Destination) (*transport.Link, error) {
		endpoint := newTestLinkEndpoint()
		done := startStressPipeEcho(endpoint)
		resourcesMu.Lock()
		endpoints = append(endpoints, endpoint)
		echoes = append(echoes, done)
		resourcesMu.Unlock()
		return endpoint.link, nil
	}}

	clientReadDone := make(chan error, 1)
	serverReadDone := make(chan error, 1)
	go func() { clientReadDone <- client.readLoop(context.Background()) }()
	go func() { serverReadDone <- server.readLoop(context.Background()) }()

	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	const streamCount = 64
	streams := make([]*stream, 0, streamCount)
	clientEndpoints := make([]*testLinkEndpoint, 0, streamCount)
	for index := range streamCount {
		endpoint := newTestLinkEndpoint()
		clientEndpoints = append(clientEndpoints, endpoint)
		stream, err := client.openStream(
			ctx,
			xnet.TCPDestination(xnet.DomainAddress(fmt.Sprintf("close-%d.example", index)), 443),
			endpoint.link,
		)
		if err != nil {
			client.close(err)
			server.close(err)
			t.Fatalf("open stream %d: %v", index, err)
		}
		streams = append(streams, stream)
		go stream.pumpUplink(client)
	}

	var closers sync.WaitGroup
	for range 8 {
		closers.Add(2)
		go func() {
			defer closers.Done()
			client.close(io.ErrClosedPipe)
		}()
		go func() {
			defer closers.Done()
			server.close(io.ErrClosedPipe)
		}()
	}
	closers.Wait()
	for _, endpoint := range clientEndpoints {
		endpoint.closeInput()
		endpoint.closeOutput()
	}
	resourcesMu.Lock()
	serverEndpoints := append([]*testLinkEndpoint(nil), endpoints...)
	echoWorkers := append([]<-chan struct{}(nil), echoes...)
	resourcesMu.Unlock()
	for _, endpoint := range serverEndpoints {
		endpoint.closeInput()
		endpoint.closeOutput()
	}

	for index, stream := range streams {
		select {
		case <-stream.done:
		case <-ctx.Done():
			t.Fatalf("stream %d did not stop during concurrent session close", index)
		}
	}
	for index, done := range echoWorkers {
		if err := waitStressChannel(ctx, fmt.Sprintf("echo worker %d", index), done); err != nil {
			t.Error(err)
		}
	}
	for name, done := range map[string]<-chan error{"client read loop": clientReadDone, "server read loop": serverReadDone} {
		select {
		case <-done:
		case <-ctx.Done():
			t.Errorf("%s did not stop", name)
		}
	}

	client.streamsMu.Lock()
	clientStreams := len(client.streams) + len(client.drainingStreams)
	client.streamsMu.Unlock()
	server.streamsMu.Lock()
	serverStreams := len(server.streams) + len(server.drainingStreams)
	server.streamsMu.Unlock()
	if clientStreams != 0 || serverStreams != 0 || client.activeStreams.Load() != 0 {
		t.Fatalf(
			"session state after close: client_streams=%d server_streams=%d active=%d",
			clientStreams,
			serverStreams,
			client.activeStreams.Load(),
		)
	}
}
