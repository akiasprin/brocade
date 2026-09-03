package anytls

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"io"
	"net"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/sagernet/sing/common/uot"
	"github.com/xtls/xray-core/common/buf"
	xnet "github.com/xtls/xray-core/common/net"
	"github.com/xtls/xray-core/transport"
	"github.com/xtls/xray-core/transport/pipe"
)

type testLinkEndpoint struct {
	link        *transport.Link
	input       *pipe.Writer
	output      *pipe.Reader
	closeInput  func()
	closeOutput func()
}

func newTestLinkEndpoint() *testLinkEndpoint {
	inputReader, inputWriter := pipe.New(pipe.WithoutSizeLimit())
	outputReader, outputWriter := pipe.New(pipe.WithoutSizeLimit())
	return &testLinkEndpoint{
		link:        &transport.Link{Reader: inputReader, Writer: outputWriter},
		input:       inputWriter,
		output:      outputReader,
		closeInput:  func() { _ = inputWriter.Close() },
		closeOutput: func() { _ = outputWriter.Close() },
	}
}

func newSessionForConn(conn net.Conn, isClient bool) *session {
	s := &session{
		isClient:    isClient,
		conn:        conn,
		br:          &buf.BufferedReader{Reader: buf.NewReader(conn)},
		bw:          buf.NewBufferedWriter(buf.NewWriter(conn)),
		streams:     make(map[uint32]*stream),
		errCh:       make(chan error, 1),
		synAckCh:    make(map[uint32]chan error),
		peerVersion: 1,
	}
	s.fw = newFrameWriter(s.bw)
	if isClient {
		s.paddingScheme = getDefaultPaddingScheme()
		s.nextSID.Store(1)
		s.pktCounter.Store(1)
	}
	return s
}

func startPipeEcho(t *testing.T, endpoint *testLinkEndpoint) {
	t.Helper()
	go func() {
		for {
			mb, err := endpoint.output.ReadMultiBuffer()
			if err != nil {
				return
			}
			if err := endpoint.input.WriteMultiBuffer(mb); err != nil {
				return
			}
		}
	}()
}

func writeEndpoint(t *testing.T, endpoint *testLinkEndpoint, payload []byte) {
	t.Helper()
	if err := endpoint.input.WriteMultiBuffer(buf.MultiBuffer{buf.FromBytes(payload)}); err != nil {
		t.Fatal(err)
	}
}

func closeSessionPair(t *testing.T, client, server *session, clientErr, serverErr <-chan error, endpoints ...*testLinkEndpoint) {
	t.Helper()
	client.close(nil)
	server.close(nil)
	for _, endpoint := range endpoints {
		endpoint.closeInput()
		endpoint.closeOutput()
	}
	for name, ch := range map[string]<-chan error{"client": clientErr, "server": serverErr} {
		select {
		case <-ch:
		case <-time.After(2 * time.Second):
			t.Fatalf("%s session read loop did not stop", name)
		}
	}
}

func TestSessionPairTCPMultiplexingAndLargeWrites(t *testing.T) {
	rawClientConn, serverConn := net.Pipe()
	client := newSessionForConn(rawClientConn, true)
	server := newSessionForConn(serverConn, false)

	clientEndpoint := newTestLinkEndpoint()
	serverEndpoint := newTestLinkEndpoint()
	secondClientEndpoint := newTestLinkEndpoint()
	secondServerEndpoint := newTestLinkEndpoint()
	var dispatchMu sync.Mutex
	var dispatchCount int
	dispatcher := &testDispatcher{dispatch: func(_ context.Context, destination xnet.Destination) (*transport.Link, error) {
		if destination.Network != xnet.Network_TCP {
			return nil, fmt.Errorf("unexpected network: %v", destination.Network)
		}
		dispatchMu.Lock()
		defer dispatchMu.Unlock()
		dispatchCount++
		switch dispatchCount {
		case 1:
			return serverEndpoint.link, nil
		case 2:
			return secondServerEndpoint.link, nil
		default:
			return nil, errors.New("unexpected extra dispatch")
		}
	}}
	server.dispatcher = dispatcher

	clientErr := make(chan error, 1)
	serverErr := make(chan error, 1)
	go func() { clientErr <- client.readLoop(context.Background()) }()
	go func() { serverErr <- server.readLoop(context.Background()) }()
	startPipeEcho(t, serverEndpoint)
	startPipeEcho(t, secondServerEndpoint)

	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	first, err := client.openStream(ctx, xnet.TCPDestination(xnet.DomainAddress("example.com"), 443), clientEndpoint.link)
	if err != nil {
		closeSessionPair(t, client, server, clientErr, serverErr, clientEndpoint, serverEndpoint, secondClientEndpoint, secondServerEndpoint)
		t.Fatal(err)
	}
	go first.pumpUplink(client)
	firstPayload := make([]byte, 2*maxFramePayload+123)
	for i := range firstPayload {
		firstPayload[i] = byte(i)
	}
	writeEndpoint(t, clientEndpoint, firstPayload)
	if got := readPipeExact(t, clientEndpoint.output, len(firstPayload)); !bytes.Equal(got, firstPayload) {
		t.Fatalf("first stream payload mismatch: got %d bytes", len(got))
	}
	clientEndpoint.closeInput()

	second, err := client.openStream(ctx, xnet.TCPDestination(xnet.IPAddress([]byte{127, 0, 0, 1}), 80), secondClientEndpoint.link)
	if err != nil {
		closeSessionPair(t, client, server, clientErr, serverErr, clientEndpoint, serverEndpoint, secondClientEndpoint, secondServerEndpoint)
		t.Fatal(err)
	}
	go second.pumpUplink(client)
	secondPayload := bytes.Repeat([]byte("second-stream"), 4096)
	writeEndpoint(t, secondClientEndpoint, secondPayload)
	if got := readPipeExact(t, secondClientEndpoint.output, len(secondPayload)); !bytes.Equal(got, secondPayload) {
		t.Fatalf("second stream payload mismatch: got %d bytes", len(got))
	}
	secondClientEndpoint.closeInput()

	select {
	case <-first.done:
	case <-time.After(2 * time.Second):
		t.Fatal("first stream did not close")
	}
	select {
	case <-second.done:
	case <-time.After(2 * time.Second):
		t.Fatal("second stream did not close")
	}
	closeSessionPair(t, client, server, clientErr, serverErr, clientEndpoint, serverEndpoint, secondClientEndpoint, secondServerEndpoint)
}

func TestSessionPairUDPOverTCP(t *testing.T) {
	clientConn, serverConn := net.Pipe()
	client := newSessionForConn(clientConn, true)
	server := newSessionForConn(serverConn, false)
	clientEndpoint := newTestLinkEndpoint()
	serverEndpoint := newTestLinkEndpoint()
	gotDestination := make(chan xnet.Destination, 1)
	server.dispatcher = &testDispatcher{dispatch: func(_ context.Context, destination xnet.Destination) (*transport.Link, error) {
		gotDestination <- destination
		return serverEndpoint.link, nil
	}}

	clientErr := make(chan error, 1)
	serverErr := make(chan error, 1)
	go func() { clientErr <- client.readLoop(context.Background()) }()
	go func() { serverErr <- server.readLoop(context.Background()) }()
	startPipeEcho(t, serverEndpoint)

	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	stream, err := client.openStream(ctx, xnet.UDPDestination(xnet.DomainAddress("dns.example"), 53), clientEndpoint.link)
	if err != nil {
		closeSessionPair(t, client, server, clientErr, serverErr, clientEndpoint, serverEndpoint)
		t.Fatal(err)
	}
	select {
	case destination := <-gotDestination:
		if destination.Network != xnet.Network_UDP || destination.Address.String() != "dns.example" || destination.Port != 53 {
			t.Fatalf("dispatched UDP destination = %v", destination)
		}
	case <-time.After(2 * time.Second):
		t.Fatal("UDP-over-TCP request was not dispatched")
	}

	go stream.pumpUplink(client)
	payload := []byte("udp-over-tcp-payload")
	writeEndpoint(t, clientEndpoint, payload)
	if got := readPipeExact(t, clientEndpoint.output, len(payload)); !bytes.Equal(got, payload) {
		t.Fatalf("UDP-over-TCP payload mismatch: got %q", got)
	}
	clientEndpoint.closeInput()
	select {
	case <-stream.done:
	case <-time.After(2 * time.Second):
		t.Fatal("UDP stream did not close")
	}
	closeSessionPair(t, client, server, clientErr, serverErr, clientEndpoint, serverEndpoint)
}

func TestClientUDPStreamDoesNotWaitForOptionalSYNACK(t *testing.T) {
	clientConn, peerConn := net.Pipe()
	client := newSessionForConn(clientConn, true)
	client.peerVersion = 2
	client.synAckSupported.Store(true)
	endpoint := newTestLinkEndpoint()

	wireCh := make(chan []byte, 1)
	go func() {
		wire, _ := io.ReadAll(peerConn)
		wireCh <- wire
	}()

	ctx, cancel := context.WithTimeout(context.Background(), 500*time.Millisecond)
	defer cancel()
	started := time.Now()
	stream, err := client.openStream(ctx, xnet.UDPDestination(xnet.DomainAddress("dns.example"), 53), endpoint.link)
	if err != nil {
		client.close(err)
		_ = peerConn.Close()
		endpoint.closeInput()
		endpoint.closeOutput()
		t.Fatalf("UDP stream open error = %v", err)
	}
	if elapsed := time.Since(started); elapsed >= 400*time.Millisecond {
		client.close(nil)
		_ = peerConn.Close()
		endpoint.closeInput()
		endpoint.closeOutput()
		t.Fatalf("UDP stream waited for SYNACK: %v", elapsed)
	}

	client.close(nil)
	_ = peerConn.Close()
	endpoint.closeInput()
	endpoint.closeOutput()
	frames := parseTestFrames(t, <-wireCh)
	var requestSeen bool
	for _, frame := range frames {
		if frame.cmd != cmdPSH || frame.sid != stream.sid {
			continue
		}
		request, requestErr := uot.ReadRequest(bytes.NewReader(frame.data))
		if requestErr == nil && request.IsConnect && request.Destination.String() == "dns.example:53" {
			requestSeen = true
			break
		}
	}
	if !requestSeen {
		t.Fatalf("UDP-over-TCP request was not sent in stream frames: %+v", frames)
	}
}

func TestSessionPairDispatcherFailureSendsSYNACKRejection(t *testing.T) {
	clientConn, serverConn := net.Pipe()
	client := newSessionForConn(clientConn, true)
	server := newSessionForConn(serverConn, false)
	client.nextSID.Store(2)
	client.peerVersion = 2
	client.synAckSupported.Store(true)
	endpoint := newTestLinkEndpoint()

	wantErr := errors.New("destination unavailable")
	server.dispatcher = &testDispatcher{dispatch: func(context.Context, xnet.Destination) (*transport.Link, error) {
		return nil, wantErr
	}}
	clientErr := make(chan error, 1)
	serverErr := make(chan error, 1)
	go func() { clientErr <- client.readLoop(context.Background()) }()
	go func() { serverErr <- server.readLoop(context.Background()) }()

	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()
	started := time.Now()
	_, err := client.openStream(ctx, xnet.TCPDestination(xnet.DomainAddress("unavailable.example"), 443), endpoint.link)
	if err == nil || !strings.Contains(err.Error(), "destination unavailable") {
		closeSessionPair(t, client, server, clientErr, serverErr, endpoint)
		t.Fatalf("openStream error = %v, want dispatcher rejection", err)
	}
	if strings.Contains(err.Error(), "SYNACK timeout") || time.Since(started) >= time.Second {
		closeSessionPair(t, client, server, clientErr, serverErr, endpoint)
		t.Fatalf("dispatcher rejection was not returned promptly: %v", err)
	}

	streamDeadline := time.NewTimer(time.Second)
	defer streamDeadline.Stop()
	for {
		server.streamsMu.Lock()
		streamCount := len(server.streams)
		server.streamsMu.Unlock()
		if streamCount == 0 {
			break
		}
		select {
		case <-streamDeadline.C:
			closeSessionPair(t, client, server, clientErr, serverErr, endpoint)
			t.Fatalf("server stream count after rejection = %d, want 0", streamCount)
		case <-time.After(time.Millisecond):
		}
	}
	closeSessionPair(t, client, server, clientErr, serverErr, endpoint)
}

func TestClientDoesNotSendStreamFrameBeforeSYN(t *testing.T) {
	clientConn, peerConn := net.Pipe()
	client := newSessionForConn(clientConn, true)
	endpoint := newTestLinkEndpoint()
	defer endpoint.closeInput()
	defer endpoint.closeOutput()

	wireCh := make(chan []byte, 1)
	go func() {
		wire, _ := io.ReadAll(peerConn)
		wireCh <- wire
	}()

	stream, err := client.openStream(context.Background(), xnet.TCPDestination(xnet.DomainAddress("example.com"), 443), endpoint.link)
	if err != nil {
		client.close(err)
		t.Fatal(err)
	}
	go stream.pumpUplink(client)
	writeEndpoint(t, endpoint, []byte("payload"))
	endpoint.closeInput()
	select {
	case <-stream.done:
	case <-time.After(2 * time.Second):
		client.close(nil)
		t.Fatal("stream did not finish")
	}
	client.close(nil)

	frames := parseTestFrames(t, <-wireCh)
	opened := make(map[uint32]bool)
	for _, frame := range frames {
		if frame.sid == 0 {
			continue
		}
		if frame.cmd == cmdSYN {
			opened[frame.sid] = true
			continue
		}
		if !opened[frame.sid] {
			t.Fatalf("command %d reached wire before SYN for stream %d", frame.cmd, frame.sid)
		}
	}
	if !opened[1] {
		t.Fatal("no stream SYN reached wire")
	}
}
