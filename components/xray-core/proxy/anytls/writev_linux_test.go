//go:build linux

package anytls

import (
	"bytes"
	"context"
	gotls "crypto/tls"
	"crypto/x509"
	"encoding/binary"
	"io"
	"testing"

	"github.com/xtls/xray-core/common/buf"
	"github.com/xtls/xray-core/common/platform"
	"github.com/xtls/xray-core/common/protocol/tls/cert"
	v2tls "github.com/xtls/xray-core/transport/internet/tls"
)

func TestWritePSHBatchVectoredUsesOneNormalBatch(t *testing.T) {
	destinationReader, destinationWriter := tcpConnPair(t)
	defer destinationReader.Close()
	defer destinationWriter.Close()
	if err := destinationWriter.SetWriteBuffer(1 << 20); err != nil {
		t.Fatal(err)
	}

	payload := make([]byte, maxPSHBatchPayloadSize)
	for index := range payload {
		payload[index] = byte(index)
	}
	data := buf.MultiBuffer{
		buf.FromBytes(payload[:1]),
		buf.FromBytes(payload[1 : buf.Size+17]),
		buf.FromBytes(payload[buf.Size+17 : maxFramePayload+9]),
		buf.FromBytes(payload[maxFramePayload+9:]),
	}
	defer buf.ReleaseMulti(data)

	wireResult := make(chan struct {
		wire []byte
		err  error
	}, 1)
	go func() {
		wire := make([]byte, len(payload)+3*frameHeaderSize)
		_, err := io.ReadFull(destinationReader, wire)
		wireResult <- struct {
			wire []byte
			err  error
		}{wire: wire, err: err}
	}()

	result := writePSHBatchVectored(mustSyscallConn(t, destinationWriter), 0x01020304, data)
	if result.err != nil {
		t.Fatal(result.err)
	}
	if !result.handled || result.payloadBytes != int64(len(payload)) || result.batches != 1 {
		t.Fatalf("writev result = %+v", result)
	}
	if result.syscalls != 1 {
		t.Fatalf("writev syscalls = %d, want 1", result.syscalls)
	}

	wire := <-wireResult
	if wire.err != nil {
		t.Fatal(wire.err)
	}
	frames := parseTestFrames(t, wire.wire)
	if len(frames) != 3 {
		t.Fatalf("frame count = %d, want 3", len(frames))
	}
	var reconstructed bytes.Buffer
	for _, frame := range frames {
		if frame.cmd != cmdPSH || frame.sid != 0x01020304 {
			t.Fatalf("unexpected frame cmd=%d sid=%08x", frame.cmd, frame.sid)
		}
		reconstructed.Write(frame.data)
	}
	if !bytes.Equal(reconstructed.Bytes(), payload) {
		t.Fatal("vectored payload differs from source")
	}
	if got := binary.BigEndian.Uint16(wire.wire[5:7]); got != maxFramePayload {
		t.Fatalf("first frame payload = %d, want %d", got, maxFramePayload)
	}
}

func TestAdvanceWritevVectorsPreservesPartialVector(t *testing.T) {
	vectors := [][]byte{[]byte("header"), []byte("payload"), []byte("tail")}
	vectors = advanceWritevVectors(vectors, len("header")+3)
	if len(vectors) != 2 || string(vectors[0]) != "load" || string(vectors[1]) != "tail" {
		t.Fatalf("advanced vectors = %q", vectors)
	}
}

func TestKernelTLSVectoredWritePreservesAnyTLSFrames(t *testing.T) {
	t.Setenv(platform.UseAnyTLSKernelTLS, "auto")
	t.Setenv(platform.UseAnyTLSWritev, "auto")
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
	sess.paddingScheme, _ = parsePaddingScheme("stop=0\n0=30-30")
	payload := make([]byte, maxPSHBatchPayloadSize)
	for index := range payload {
		payload[index] = byte(index)
	}
	wireResult := make(chan struct {
		wire []byte
		err  error
	}, 1)
	go func() {
		wire := make([]byte, len(payload)+3*frameHeaderSize)
		_, readErr := io.ReadFull(clientTLS, wire)
		wireResult <- struct {
			wire []byte
			err  error
		}{wire: wire, err: readErr}
	}()
	if err := sess.sendStreamData(21, buf.MultiBuffer{buf.FromBytes(payload)}); err != nil {
		t.Fatal(err)
	}
	wire := <-wireResult
	if wire.err != nil {
		t.Fatal(wire.err)
	}
	frames := parseTestFrames(t, wire.wire)
	if len(frames) != 3 {
		t.Fatalf("frame count = %d, want 3", len(frames))
	}
	var reconstructed bytes.Buffer
	for _, frame := range frames {
		reconstructed.Write(frame.data)
	}
	if !bytes.Equal(reconstructed.Bytes(), payload) {
		t.Fatal("kTLS writev payload differs from source")
	}
}
