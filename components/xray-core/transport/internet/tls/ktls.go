package tls

import (
	"bytes"
	"context"
	"crypto/hkdf"
	"crypto/sha256"
	"crypto/sha512"
	gotls "crypto/tls"
	"encoding/binary"
	"encoding/hex"
	stderrors "errors"
	"fmt"
	"hash"
	"io"
	"strings"
	"sync"

	"github.com/xtls/xray-core/common/errors"
	"github.com/xtls/xray-core/common/net"
	"github.com/xtls/xray-core/common/platform"
	"github.com/xtls/xray-core/transport/internet/stat"
)

var (
	errKernelTLSUnavailable     = stderrors.New("kernel TLS is unavailable")
	errKernelTLSUnsupportedConn = stderrors.New("connection does not expose a raw socket")
)

type kernelTLSKeyMaterial struct {
	cipherSuite uint16
	txKey       []byte
	txIV        []byte
	rxKey       []byte
	rxIV        []byte
}

func (m *kernelTLSKeyMaterial) clear() {
	clear(m.txKey)
	clear(m.txIV)
	clear(m.rxKey)
	clear(m.rxIV)
}

// TryEnableKernelTLS promotes a server-side TLS connection before its first
// application read. In auto mode unsupported platforms and cipher suites keep
// using Go TLS. Handshake failures and failures after the kernel socket has
// been modified remain fatal.
func TryEnableKernelTLS(ctx context.Context, conn net.Conn) (bool, error) {
	mode := strings.ToLower(strings.TrimSpace(platform.NewEnvFlag(platform.UseAnyTLSKernelTLS).GetValue(func() string {
		return "auto"
	})))
	if mode == "" {
		mode = "auto"
	}
	if mode == "off" || mode == "false" || mode == "0" || mode == "disabled" {
		return false, nil
	}
	required := mode == "required"
	if mode != "auto" && !required && mode != "on" && mode != "true" && mode != "1" {
		return false, fmt.Errorf("invalid %s mode %q", platform.UseAnyTLSKernelTLS, mode)
	}

	inner := conn
	if counterConn, ok := inner.(*stat.CounterConnection); ok {
		inner = counterConn.Connection
	}
	tlsConn, ok := inner.(*Conn)
	if !ok {
		if required {
			return false, fmt.Errorf("%w: AnyTLS connection is not direct Go TLS", errKernelTLSUnavailable)
		}
		return false, nil
	}

	enabled, err := tlsConn.enableKernelTLS(ctx)
	if err == nil {
		return enabled, nil
	}
	if stderrors.Is(err, errKernelTLSUnavailable) && !required {
		errors.LogDebug(ctx, "AnyTLS kTLS fallback: ", err)
		return false, nil
	}
	return false, err
}

// KernelTLSRawConn returns the plaintext kernel-TLS socket after promotion.
// Callers must preserve protocol framing and serialize it with ordinary writes.
func KernelTLSRawConn(conn net.Conn) (net.Conn, bool) {
	inner := conn
	if counterConn, ok := inner.(*stat.CounterConnection); ok {
		inner = counterConn.Connection
	}
	tlsConn, ok := inner.(*Conn)
	if !ok || tlsConn.kernelTLSConn() == nil {
		return nil, false
	}
	return tlsConn.rawConn, true
}

func (c *Conn) enableKernelTLS(ctx context.Context) (bool, error) {
	c.promoteMu.Lock()
	defer c.promoteMu.Unlock()

	if c.kernelTLSConn() != nil {
		return true, nil
	}
	if c.serverConfig == nil || c.rawConn == nil {
		return false, fmt.Errorf("%w: not a server-side TLS connection", errKernelTLSUnavailable)
	}
	if c.ioStarted.Load() {
		return false, fmt.Errorf("%w: TLS application I/O has already started", errKernelTLSUnavailable)
	}
	if err := preflightKernelTLS(); err != nil {
		return false, fmt.Errorf("%w: %v", errKernelTLSUnavailable, err)
	}

	secrets := newTrafficSecretCapture()
	config := c.serverConfig.Clone()
	// A NewSessionTicket consumes an application-data record sequence number.
	// Disabling tickets keeps both directions at sequence zero when kTLS takes
	// ownership after the handshake. AnyTLS does not rely on TLS resumption.
	config.SessionTicketsDisabled = true
	if config.KeyLogWriter == nil {
		config.KeyLogWriter = secrets
	} else {
		config.KeyLogWriter = io.MultiWriter(config.KeyLogWriter, secrets)
	}

	boundaryConn := newTLSRecordBoundaryConn(c.rawConn)
	tlsConn := gotls.Server(boundaryConn, config)
	c.stateMu.Lock()
	c.Conn = tlsConn
	c.stateMu.Unlock()

	if err := tlsConn.HandshakeContext(ctx); err != nil {
		secrets.clear()
		return false, err
	}
	c.ioStarted.Store(true)

	state := tlsConn.ConnectionState()
	if state.Version != gotls.VersionTLS13 {
		secrets.clear()
		return false, fmt.Errorf("%w: negotiated TLS version 0x%x", errKernelTLSUnavailable, state.Version)
	}
	clientSecret, serverSecret, ok := secrets.takeTrafficSecrets()
	secrets.clear()
	if !ok {
		clear(clientSecret)
		clear(serverSecret)
		return false, fmt.Errorf("%w: TLS 1.3 traffic secrets were not captured", errKernelTLSUnavailable)
	}
	defer clear(clientSecret)
	defer clear(serverSecret)

	material, err := deriveKernelTLSKeyMaterial(state.CipherSuite, serverSecret, clientSecret)
	if err != nil {
		return false, fmt.Errorf("%w: %v", errKernelTLSUnavailable, err)
	}
	defer material.clear()
	modified, err := installKernelTLS(c.rawConn, material)
	if err != nil {
		if modified {
			_ = c.rawConn.Close()
			return false, fmt.Errorf("install kTLS after enabling TCP ULP: %w", err)
		}
		return false, fmt.Errorf("%w: %v", errKernelTLSUnavailable, err)
	}

	c.stateMu.Lock()
	c.kernelConn = newKernelTLSConn(c.rawConn)
	c.stateMu.Unlock()
	return true, nil
}

func deriveKernelTLSKeyMaterial(cipherSuite uint16, txSecret, rxSecret []byte) (*kernelTLSKeyMaterial, error) {
	var (
		hashFunc func() hash.Hash
		keyLen   int
	)
	switch cipherSuite {
	case gotls.TLS_AES_128_GCM_SHA256:
		hashFunc = sha256.New
		keyLen = 16
	case gotls.TLS_AES_256_GCM_SHA384:
		hashFunc = sha512.New384
		keyLen = 32
	default:
		return nil, fmt.Errorf("unsupported TLS 1.3 cipher suite 0x%x", cipherSuite)
	}
	if len(txSecret) != hashFunc().Size() || len(rxSecret) != hashFunc().Size() {
		return nil, fmt.Errorf("invalid traffic secret size for cipher suite 0x%x", cipherSuite)
	}

	txKey, err := expandTLS13Label(hashFunc, txSecret, "key", keyLen)
	if err != nil {
		return nil, err
	}
	txIV, err := expandTLS13Label(hashFunc, txSecret, "iv", 12)
	if err != nil {
		clear(txKey)
		return nil, err
	}
	rxKey, err := expandTLS13Label(hashFunc, rxSecret, "key", keyLen)
	if err != nil {
		clear(txKey)
		clear(txIV)
		return nil, err
	}
	rxIV, err := expandTLS13Label(hashFunc, rxSecret, "iv", 12)
	if err != nil {
		clear(txKey)
		clear(txIV)
		clear(rxKey)
		return nil, err
	}

	return &kernelTLSKeyMaterial{
		cipherSuite: cipherSuite,
		txKey:       txKey,
		txIV:        txIV,
		rxKey:       rxKey,
		rxIV:        rxIV,
	}, nil
}

func expandTLS13Label(hashFunc func() hash.Hash, secret []byte, label string, length int) ([]byte, error) {
	fullLabel := "tls13 " + label
	info := make([]byte, 2+1+len(fullLabel)+1)
	binary.BigEndian.PutUint16(info[:2], uint16(length))
	info[2] = byte(len(fullLabel))
	copy(info[3:], fullLabel)
	// The final byte is the zero-length context.
	return hkdf.Expand(hashFunc, secret, string(info), length)
}

type trafficSecretCapture struct {
	mu     sync.Mutex
	buffer []byte
	client []byte
	server []byte
}

func newTrafficSecretCapture() *trafficSecretCapture {
	return &trafficSecretCapture{}
}

func (c *trafficSecretCapture) Write(p []byte) (int, error) {
	c.mu.Lock()
	defer c.mu.Unlock()

	c.buffer = append(c.buffer, p...)
	for {
		newline := bytes.IndexByte(c.buffer, '\n')
		if newline < 0 {
			break
		}
		c.consumeLine(c.buffer[:newline])
		copy(c.buffer, c.buffer[newline+1:])
		clear(c.buffer[len(c.buffer)-(newline+1):])
		c.buffer = c.buffer[:len(c.buffer)-(newline+1)]
	}
	return len(p), nil
}

func (c *trafficSecretCapture) consumeLine(line []byte) {
	fields := bytes.Fields(line)
	if len(fields) != 3 {
		return
	}
	var target *[]byte
	switch {
	case bytes.Equal(fields[0], []byte("CLIENT_TRAFFIC_SECRET_0")):
		target = &c.client
	case bytes.Equal(fields[0], []byte("SERVER_TRAFFIC_SECRET_0")):
		target = &c.server
	default:
		return
	}
	secret := make([]byte, hex.DecodedLen(len(fields[2])))
	n, err := hex.Decode(secret, fields[2])
	if err != nil {
		clear(secret)
		return
	}
	secret = secret[:n]
	clear(*target)
	*target = secret
}

func (c *trafficSecretCapture) takeTrafficSecrets() (client, server []byte, ok bool) {
	c.mu.Lock()
	defer c.mu.Unlock()

	client = c.client
	server = c.server
	c.client = nil
	c.server = nil
	return client, server, len(client) > 0 && len(server) > 0
}

func (c *trafficSecretCapture) clear() {
	c.mu.Lock()
	defer c.mu.Unlock()

	clear(c.buffer)
	clear(c.client)
	clear(c.server)
	c.buffer = nil
	c.client = nil
	c.server = nil
}
