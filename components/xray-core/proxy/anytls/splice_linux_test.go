//go:build linux

package anytls

import (
	"bytes"
	"context"
	"encoding/binary"
	"io"
	"net"
	"syscall"
	"testing"

	"github.com/xtls/xray-core/common/buf"
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
	splicer := &framedDownlinkSplicer{session: sess, sid: 0x01020304}
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
