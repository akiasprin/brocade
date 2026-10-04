//go:build linux

package anytls

import (
	"bytes"
	"context"
	gotls "crypto/tls"
	"crypto/x509"
	"encoding/binary"
	"io"
	"net"
	"syscall"
	"testing"
	"time"

	M "github.com/sagernet/sing/common/metadata"
	"github.com/xtls/xray-core/common/buf"
	xnet "github.com/xtls/xray-core/common/net"
	"github.com/xtls/xray-core/common/platform"
	"github.com/xtls/xray-core/common/protocol"
	"github.com/xtls/xray-core/common/protocol/tls/cert"
	sessionctx "github.com/xtls/xray-core/common/session"
	"github.com/xtls/xray-core/common/singbridge"
	"github.com/xtls/xray-core/transport"
	v2tls "github.com/xtls/xray-core/transport/internet/tls"
	"golang.org/x/sys/unix"
)

func TestWriteSplicedFramePreservesAnyTLSWireFormat(t *testing.T) {
	destinationReader, destinationWriter := tcpConnPair(t)
	defer destinationReader.Close()
	defer destinationWriter.Close()

	sess := &session{
		conn: destinationWriter,
		bw:   buf.NewBufferedWriter(buf.NewWriter(destinationWriter)),
	}
	sess.fw = newFrameWriter(sess.bw)
	splicer := &framedDownlinkSplicer{session: sess, sid: 0x01020304, useSplice: true}
	destinationRaw := mustSyscallConn(t, destinationWriter)

	pipeFDs := [2]int{-1, -1}
	if err := unix.Pipe2(pipeFDs[:], unix.O_CLOEXEC|unix.O_NONBLOCK); err != nil {
		t.Fatal(err)
	}
	defer unix.Close(pipeFDs[0])
	defer unix.Close(pipeFDs[1])
	payload := []byte("framed splice payload")
	if n, err := unix.Write(pipeFDs[1], payload); err != nil || n != len(payload) {
		t.Fatalf("pipe write = (%d, %v), want (%d, nil)", n, err, len(payload))
	}

	wireResult := make(chan struct {
		wire []byte
		err  error
	}, 1)
	go func() {
		wire := make([]byte, frameHeaderSize+len(payload))
		_, err := io.ReadFull(destinationReader, wire)
		wireResult <- struct {
			wire []byte
			err  error
		}{wire: wire, err: err}
	}()
	if err := splicer.writeSplicedFrame(context.Background(), destinationRaw, pipeFDs[0], len(payload)); err != nil {
		t.Fatal(err)
	}
	result := <-wireResult
	if result.err != nil {
		t.Fatal(result.err)
	}
	if result.wire[0] != cmdPSH || binary.BigEndian.Uint32(result.wire[1:5]) != splicer.sid ||
		int(binary.BigEndian.Uint16(result.wire[5:7])) != len(payload) {
		t.Fatalf("unexpected AnyTLS frame header %x", result.wire[:frameHeaderSize])
	}
	if !bytes.Equal(result.wire[frameHeaderSize:], payload) {
		t.Fatal("AnyTLS frame payload differs from spliced source")
	}
}

func TestSpliceSocketThroughPipe(t *testing.T) {
	sourceReader, sourceWriter := tcpConnPair(t)
	defer sourceReader.Close()
	defer sourceWriter.Close()
	destinationReader, destinationWriter := tcpConnPair(t)
	defer destinationReader.Close()
	defer destinationWriter.Close()

	payload := bytes.Repeat([]byte("anytls-splice-"), 32*1024)
	writeResult := make(chan error, 1)
	go func() {
		_, err := sourceWriter.Write(payload)
		if closeErr := sourceWriter.CloseWrite(); err == nil {
			err = closeErr
		}
		writeResult <- err
	}()
	readResult := make(chan struct {
		payload []byte
		err     error
	}, 1)
	go func() {
		body, err := io.ReadAll(destinationReader)
		readResult <- struct {
			payload []byte
			err     error
		}{payload: body, err: err}
	}()

	sourceRaw := mustSyscallConn(t, sourceReader)
	destinationRaw := mustSyscallConn(t, destinationWriter)
	pipeFDs := [2]int{-1, -1}
	if err := unix.Pipe2(pipeFDs[:], unix.O_CLOEXEC|unix.O_NONBLOCK); err != nil {
		t.Fatal(err)
	}
	defer unix.Close(pipeFDs[0])
	defer unix.Close(pipeFDs[1])
	_, _ = unix.FcntlInt(uintptr(pipeFDs[1]), unix.F_SETPIPE_SZ, maxFramePayload+1)

	var copied int
	for {
		length, err := spliceSocketToPipe(sourceRaw, pipeFDs[1], maxFramePayload)
		if err != nil {
			t.Fatal(err)
		}
		if length == 0 {
			break
		}
		if err := splicePipeToSocket(destinationRaw, pipeFDs[0], length); err != nil {
			t.Fatal(err)
		}
		copied += length
	}
	if err := destinationWriter.CloseWrite(); err != nil {
		t.Fatal(err)
	}
	if err := <-writeResult; err != nil {
		t.Fatal(err)
	}
	result := <-readResult
	if result.err != nil {
		t.Fatal(result.err)
	}
	if copied != len(payload) {
		t.Fatalf("copied %d bytes, want %d", copied, len(payload))
	}
	if !bytes.Equal(result.payload, payload) {
		t.Fatal("spliced payload differs from source")
	}
}

func TestKernelTLSFramedSpliceFlushesShortFrame(t *testing.T) {
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

	serverRaw, clientRaw := tcpConnPair(t)
	serverTLS := v2tls.Server(serverRaw, &gotls.Config{
		Certificates: []gotls.Certificate{keyPair},
		MinVersion:   gotls.VersionTLS13,
		MaxVersion:   gotls.VersionTLS13,
	})
	clientTLS := gotls.Client(clientRaw, &gotls.Config{
		InsecureSkipVerify: true,
		ServerName:         "localhost",
		MinVersion:         gotls.VersionTLS13,
		MaxVersion:         gotls.VersionTLS13,
	})
	defer serverTLS.Close()
	defer clientTLS.Close()

	type enableResult struct {
		enabled bool
		err     error
	}
	enabledResult := make(chan enableResult, 1)
	go func() {
		enabled, enableErr := v2tls.TryEnableKernelTLS(context.Background(), serverTLS)
		enabledResult <- enableResult{enabled: enabled, err: enableErr}
	}()
	clientHandshake := make(chan error, 1)
	go func() {
		clientHandshake <- clientTLS.Handshake()
	}()
	result := <-enabledResult
	if result.err != nil {
		t.Fatal(result.err)
	}
	if !result.enabled {
		// The availability preflight may reject kTLS before the server starts its
		// handshake. Close the pair so the concurrent client handshake can exit.
		_ = serverRaw.Close()
		_ = clientRaw.Close()
		<-clientHandshake
		t.Skip("kTLS is unavailable on this kernel")
	}
	if err := <-clientHandshake; err != nil {
		t.Fatal(err)
	}

	sess := &session{
		conn: serverTLS,
		bw:   buf.NewBufferedWriter(buf.NewWriter(serverTLS)),
	}
	sess.fw = newFrameWriter(sess.bw)
	splicer := &framedDownlinkSplicer{session: sess, sid: 0x01020304, useSplice: true}
	sourceReader, sourceWriter := tcpConnPair(t)
	defer sourceReader.Close()
	defer sourceWriter.Close()

	payload := []byte("short origin response")
	go func() {
		_, _ = sourceWriter.Write(payload)
		_ = sourceWriter.CloseWrite()
	}()
	spliceResult := make(chan struct {
		handled bool
		err     error
	}, 1)
	go func() {
		handled, spliceErr := splicer.SpliceDownlink(context.Background(), sourceReader, nil)
		spliceResult <- struct {
			handled bool
			err     error
		}{handled: handled, err: spliceErr}
	}()

	if err := clientTLS.SetReadDeadline(time.Now().Add(2 * time.Second)); err != nil {
		t.Fatal(err)
	}
	wire := make([]byte, frameHeaderSize+len(payload))
	if _, err := io.ReadFull(clientTLS, wire); err != nil {
		t.Fatal(err)
	}
	if wire[0] != cmdPSH || binary.BigEndian.Uint32(wire[1:5]) != splicer.sid ||
		int(binary.BigEndian.Uint16(wire[5:7])) != len(payload) {
		t.Fatalf("unexpected AnyTLS frame header %x", wire[:frameHeaderSize])
	}
	if !bytes.Equal(wire[frameHeaderSize:], payload) {
		t.Fatal("kTLS spliced payload differs from source")
	}
	spliced := <-spliceResult
	if !spliced.handled || spliced.err != nil {
		t.Fatalf("SpliceDownlink() = (%v, %v), want (true, nil)", spliced.handled, spliced.err)
	}
}

func TestKernelTLSFramedWritevBypassesTransportPipe(t *testing.T) {
	serverTLS, clientTLS := kernelTLSTestPair(t)
	defer serverTLS.Close()
	defer clientTLS.Close()

	sess := &session{
		conn: serverTLS,
		bw:   buf.NewBufferedWriter(buf.NewWriter(serverTLS)),
	}
	sess.fw = newFrameWriter(sess.bw)
	sess.paddingScheme, _ = parsePaddingScheme("stop=0\n0=30-30")
	splicer := &framedDownlinkSplicer{session: sess, sid: 0x01020304}
	sourceReader, sourceWriter := tcpConnPair(t)
	defer sourceReader.Close()
	defer sourceWriter.Close()

	payload := make([]byte, maxPSHBatchPayloadSize)
	for index := range payload {
		payload[index] = byte(index)
	}
	writeResult := make(chan error, 1)
	go func() {
		_, writeErr := sourceWriter.Write(payload)
		if closeErr := sourceWriter.CloseWrite(); writeErr == nil {
			writeErr = closeErr
		}
		writeResult <- writeErr
	}()

	var accountedBytes int64
	fastPathResult := make(chan struct {
		handled bool
		err     error
	}, 1)
	go func() {
		handled, writeErr := splicer.SpliceDownlink(context.Background(), sourceReader, func(bytes int64) {
			accountedBytes += bytes
		})
		fastPathResult <- struct {
			handled bool
			err     error
		}{handled: handled, err: writeErr}
	}()

	if err := clientTLS.SetReadDeadline(time.Now().Add(2 * time.Second)); err != nil {
		t.Fatal(err)
	}
	var reconstructed bytes.Buffer
	for reconstructed.Len() < len(payload) {
		header := make([]byte, frameHeaderSize)
		if _, err := io.ReadFull(clientTLS, header); err != nil {
			t.Fatal(err)
		}
		if header[0] != cmdPSH || binary.BigEndian.Uint32(header[1:5]) != splicer.sid {
			t.Fatalf("unexpected AnyTLS frame header %x", header)
		}
		body := make([]byte, int(binary.BigEndian.Uint16(header[5:7])))
		if _, err := io.ReadFull(clientTLS, body); err != nil {
			t.Fatal(err)
		}
		reconstructed.Write(body)
	}
	if err := <-writeResult; err != nil {
		t.Fatal(err)
	}
	result := <-fastPathResult
	if !result.handled || result.err != nil {
		t.Fatalf("SpliceDownlink() = (%v, %v), want (true, nil)", result.handled, result.err)
	}
	if accountedBytes != int64(len(payload)) {
		t.Fatalf("accounted bytes = %d, want %d", accountedBytes, len(payload))
	}
	if !bytes.Equal(reconstructed.Bytes(), payload) {
		t.Fatal("kTLS direct writev payload differs from source")
	}
}

func TestFramedFastPathDoesNotBypassActivePadding(t *testing.T) {
	sess := &session{}
	sess.paddingScheme, _ = parsePaddingScheme("stop=8\n0=30-30")
	splicer := &framedDownlinkSplicer{session: sess, sid: 1}
	sourceReader, sourceWriter := tcpConnPair(t)
	defer sourceReader.Close()
	defer sourceWriter.Close()

	handled, err := splicer.SpliceDownlink(context.Background(), sourceReader, nil)
	if err != nil || handled {
		t.Fatalf("SpliceDownlink() = (%v, %v), want (false, nil)", handled, err)
	}
}

func TestKernelTLSFastPathPreservesDispatchMetadata(t *testing.T) {
	t.Setenv(platform.UseAnyTLSWritev, "auto")
	serverTLS, clientTLS := kernelTLSTestPair(t)
	defer serverTLS.Close()
	defer clientTLS.Close()

	endpoint := newTestLinkEndpoint()
	defer endpoint.closeInput()
	defer endpoint.closeOutput()

	type dispatchCapture struct {
		inbound     *sessionctx.Inbound
		content     *sessionctx.Content
		destination xnet.Destination
	}
	dispatched := make(chan dispatchCapture, 1)
	sess := newSessionForConn(serverTLS, false)
	sess.dispatcher = &testDispatcher{dispatch: func(ctx context.Context, destination xnet.Destination) (*transport.Link, error) {
		dispatched <- dispatchCapture{
			inbound:     sessionctx.InboundFromContext(ctx),
			content:     sessionctx.ContentFromContext(ctx),
			destination: destination,
		}
		return endpoint.link, nil
	}}
	defer sess.close(nil)

	user := &protocol.MemoryUser{Email: "fast-path-regression@example.com"}
	originalInbound := &sessionctx.Inbound{
		Tag:    "anytls-in",
		Name:   "anytls",
		Source: xnet.TCPDestination(xnet.ParseAddress("192.0.2.40"), 12345),
		User:   user,
	}
	originalInbound.CanSpliceCopy.Store(sessionctx.SpliceCopyDisabled)
	content := &sessionctx.Content{SniffingRequest: sessionctx.SniffingRequest{
		Enabled:                        true,
		RouteOnly:                      true,
		OverrideDestinationForProtocol: []string{"tls"},
	}}
	ctx := sessionctx.ContextWithInbound(context.Background(), originalInbound)
	ctx = sessionctx.ContextWithContent(ctx, content)

	destination := xnet.TCPDestination(xnet.DomainAddress("dispatch.example"), 443)
	address := buf.New()
	defer address.Release()
	if err := M.SocksaddrSerializer.WriteAddrPort(address, singbridge.ToSocksaddr(destination)); err != nil {
		t.Fatal(err)
	}
	stream := newStream(1, nil)
	sess.streams[stream.sid] = stream
	reader := &buf.BufferedReader{Reader: buf.NewReader(bytes.NewReader(address.Bytes()))}
	if err := sess.handleNewStream(ctx, stream, reader, int(address.Len())); err != nil {
		t.Fatal(err)
	}

	if err := clientTLS.SetReadDeadline(time.Now().Add(2 * time.Second)); err != nil {
		t.Fatal(err)
	}
	synAck := make([]byte, frameHeaderSize)
	if _, err := io.ReadFull(clientTLS, synAck); err != nil {
		t.Fatal(err)
	}
	if synAck[0] != cmdSYNACK || binary.BigEndian.Uint32(synAck[1:5]) != stream.sid {
		t.Fatalf("unexpected SYNACK frame %x", synAck)
	}

	select {
	case capture := <-dispatched:
		if capture.inbound == nil || capture.inbound == originalInbound {
			t.Fatal("fast path did not install per-stream inbound metadata")
		}
		if capture.inbound.Tag != originalInbound.Tag || capture.inbound.Name != originalInbound.Name ||
			capture.inbound.Source != originalInbound.Source || capture.inbound.User != user {
			t.Fatalf("cloned inbound metadata changed: %+v", capture.inbound)
		}
		if capture.inbound.FramedDownlinkSplicer == nil {
			t.Fatal("per-stream framed downlink splicer is missing")
		}
		if capture.inbound.CanSpliceCopy.Load() != sessionctx.SpliceCopyDirect {
			t.Fatalf("per-stream splice state = %v, want direct", capture.inbound.CanSpliceCopy.Load())
		}
		if capture.content != content || !capture.content.SniffingRequest.Enabled || !capture.content.SniffingRequest.RouteOnly {
			t.Fatal("fast path changed the sniffing request context")
		}
		if capture.destination != destination {
			t.Fatalf("dispatch destination = %v, want %v", capture.destination, destination)
		}
	case <-time.After(time.Second):
		t.Fatal("dispatcher did not receive AnyTLS stream")
	}
	if originalInbound.FramedDownlinkSplicer != nil || originalInbound.CanSpliceCopy.Load() != sessionctx.SpliceCopyDisabled {
		t.Fatal("fast path mutated connection-wide inbound metadata")
	}
}

func kernelTLSTestPair(t *testing.T) (net.Conn, *gotls.Conn) {
	t.Helper()
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

	serverRaw, clientRaw := tcpConnPair(t)
	serverTLS := v2tls.Server(serverRaw, &gotls.Config{
		Certificates: []gotls.Certificate{keyPair},
		MinVersion:   gotls.VersionTLS13,
		MaxVersion:   gotls.VersionTLS13,
	})
	clientTLS := gotls.Client(clientRaw, &gotls.Config{
		InsecureSkipVerify: true,
		ServerName:         "localhost",
		MinVersion:         gotls.VersionTLS13,
		MaxVersion:         gotls.VersionTLS13,
	})

	type enableResult struct {
		enabled bool
		err     error
	}
	enabledResult := make(chan enableResult, 1)
	go func() {
		enabled, enableErr := v2tls.TryEnableKernelTLS(context.Background(), serverTLS)
		enabledResult <- enableResult{enabled: enabled, err: enableErr}
	}()
	clientHandshake := make(chan error, 1)
	go func() {
		clientHandshake <- clientTLS.Handshake()
	}()
	result := <-enabledResult
	if result.err != nil {
		t.Fatal(result.err)
	}
	if !result.enabled {
		_ = serverRaw.Close()
		_ = clientRaw.Close()
		<-clientHandshake
		t.Skip("kTLS is unavailable on this kernel")
	}
	if err := <-clientHandshake; err != nil {
		t.Fatal(err)
	}
	return serverTLS, clientTLS
}

func tcpConnPair(t *testing.T) (*net.TCPConn, *net.TCPConn) {
	t.Helper()
	listener, err := net.ListenTCP("tcp4", &net.TCPAddr{IP: net.IPv4(127, 0, 0, 1)})
	if err != nil {
		t.Fatal(err)
	}
	defer listener.Close()
	accepted := make(chan struct {
		conn *net.TCPConn
		err  error
	}, 1)
	go func() {
		conn, acceptErr := listener.AcceptTCP()
		accepted <- struct {
			conn *net.TCPConn
			err  error
		}{conn: conn, err: acceptErr}
	}()
	client, err := net.DialTCP("tcp4", nil, listener.Addr().(*net.TCPAddr))
	if err != nil {
		t.Fatal(err)
	}
	server := <-accepted
	if server.err != nil {
		client.Close()
		t.Fatal(server.err)
	}
	return server.conn, client
}

func mustSyscallConn(t *testing.T, conn syscall.Conn) syscall.RawConn {
	t.Helper()
	rawConn, err := conn.SyscallConn()
	if err != nil {
		t.Fatal(err)
	}
	return rawConn
}
