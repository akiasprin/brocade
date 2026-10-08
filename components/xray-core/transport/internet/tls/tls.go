package tls

import (
	"bytes"
	"context"
	"crypto/rand"
	"crypto/tls"
	"math/big"
	"sync"
	"sync/atomic"
	"time"

	utls "github.com/refraction-networking/utls"
	"github.com/xtls/xray-core/common/buf"
	"github.com/xtls/xray-core/common/net"
	"github.com/xtls/xray-core/common/utils"
)

type Interface interface {
	net.Conn
	HandshakeContext(ctx context.Context) error
	VerifyHostname(host string) error
	HandshakeContextServerName(ctx context.Context) string
	NegotiatedProtocol() string
}

var _ buf.Writer = (*Conn)(nil)
var _ Interface = (*Conn)(nil)

type Conn struct {
	*tls.Conn

	rawConn      net.Conn
	serverConfig *tls.Config

	stateMu    sync.RWMutex
	kernelConn net.Conn
	ioStarted  atomic.Bool
	promoteMu  sync.Mutex

	suppressCloseNotify atomic.Bool
}

const tlsCloseTimeout = 250 * time.Millisecond

func (c *Conn) SuppressCloseNotify() {
	c.suppressCloseNotify.Store(true)
}

func (c *Conn) Close() error {
	if c.suppressCloseNotify.Load() {
		return c.rawConn.Close()
	}
	if c.kernelTLSConn() != nil {
		return closeKernelTLS(c.rawConn)
	}
	timer := time.AfterFunc(tlsCloseTimeout, func() {
		c.Conn.NetConn().Close()
	})
	defer timer.Stop()
	return c.Conn.Close()
}

func (c *Conn) Read(p []byte) (int, error) {
	if conn := c.kernelTLSConn(); conn != nil {
		return conn.Read(p)
	}
	c.ioStarted.Store(true)
	return c.tlsConn().Read(p)
}

func (c *Conn) Write(p []byte) (int, error) {
	if conn := c.kernelTLSConn(); conn != nil {
		return conn.Write(p)
	}
	c.ioStarted.Store(true)
	return c.tlsConn().Write(p)
}

func (c *Conn) SetDeadline(t time.Time) error {
	if conn := c.kernelTLSConn(); conn != nil {
		return conn.SetDeadline(t)
	}
	return c.tlsConn().SetDeadline(t)
}

func (c *Conn) SetReadDeadline(t time.Time) error {
	if conn := c.kernelTLSConn(); conn != nil {
		return conn.SetReadDeadline(t)
	}
	return c.tlsConn().SetReadDeadline(t)
}

func (c *Conn) SetWriteDeadline(t time.Time) error {
	if conn := c.kernelTLSConn(); conn != nil {
		return conn.SetWriteDeadline(t)
	}
	return c.tlsConn().SetWriteDeadline(t)
}

func (c *Conn) LocalAddr() net.Addr {
	return c.rawConn.LocalAddr()
}

func (c *Conn) RemoteAddr() net.Addr {
	return c.rawConn.RemoteAddr()
}

func (c *Conn) NetConn() net.Conn {
	return c.rawConn
}

func (c *Conn) HandshakeContext(ctx context.Context) error {
	if c.kernelTLSConn() != nil {
		return nil
	}
	c.ioStarted.Store(true)
	return c.tlsConn().HandshakeContext(ctx)
}

func (c *Conn) Handshake() error {
	return c.HandshakeContext(context.Background())
}

func (c *Conn) ConnectionState() tls.ConnectionState {
	return c.tlsConn().ConnectionState()
}

func (c *Conn) tlsConn() *tls.Conn {
	c.stateMu.RLock()
	conn := c.Conn
	c.stateMu.RUnlock()
	return conn
}

func (c *Conn) kernelTLSConn() net.Conn {
	c.stateMu.RLock()
	conn := c.kernelConn
	c.stateMu.RUnlock()
	return conn
}

func (c *Conn) WriteMultiBuffer(mb buf.MultiBuffer) error {
	mb = buf.Compact(mb)
	mb, err := buf.WriteMultiBuffer(c, mb)
	buf.ReleaseMulti(mb)
	return err
}

func (c *Conn) HandshakeContextServerName(ctx context.Context) string {
	if err := c.HandshakeContext(ctx); err != nil {
		return ""
	}
	return c.ConnectionState().ServerName
}

func (c *Conn) NegotiatedProtocol() string {
	state := c.ConnectionState()
	return state.NegotiatedProtocol
}

// VisionBuffers exposes TLS read-ahead through the transport wrapper instead
// of making protocol handlers depend on crypto/tls' private struct layout.
func (c *Conn) VisionBuffers() (*bytes.Reader, *bytes.Buffer) {
	tlsConn := c.tlsConn()
	input, inputOK := utils.TryAccessField[bytes.Reader](tlsConn, "input")
	rawInput, rawInputOK := utils.TryAccessField[bytes.Buffer](tlsConn, "rawInput")
	if !inputOK || !rawInputOK {
		return nil, nil
	}
	return input, rawInput
}

// Client initiates a TLS client handshake on the given connection.
func Client(c net.Conn, config *tls.Config) net.Conn {
	tlsConn := tls.Client(c, config)
	return &Conn{Conn: tlsConn, rawConn: c}
}

// Server initiates a TLS server handshake on the given connection.
func Server(c net.Conn, config *tls.Config) net.Conn {
	tlsConn := tls.Server(c, config)
	return &Conn{Conn: tlsConn, rawConn: c, serverConfig: config}
}

type UConn struct {
	*utls.UConn
	suppressCloseNotify atomic.Bool
}

var _ Interface = (*UConn)(nil)

func (c *UConn) SuppressCloseNotify() {
	c.suppressCloseNotify.Store(true)
}

func (c *UConn) Close() error {
	if c.suppressCloseNotify.Load() {
		return c.Conn.NetConn().Close()
	}
	timer := time.AfterFunc(tlsCloseTimeout, func() {
		c.Conn.NetConn().Close()
	})
	defer timer.Stop()
	return c.Conn.Close()
}

func (c *UConn) HandshakeContextServerName(ctx context.Context) string {
	if err := c.HandshakeContext(ctx); err != nil {
		return ""
	}
	return c.ConnectionState().ServerName
}

// WebsocketHandshake basically calls UConn.Handshake inside it but it will only send
// http/1.1 in its ALPN.
func (c *UConn) WebsocketHandshakeContext(ctx context.Context) error {
	// Build the handshake state. This will apply every variable of the TLS of the
	// fingerprint in the UConn
	if err := c.BuildHandshakeState(); err != nil {
		return err
	}
	config := *utils.AccessField[*utls.Config](c, "config")
	// Do not modify outer ALPN to http/1.1 if ECH is used
	// Outer ALPN will be h2,http/1.1, and real ALPN in config will be hidden in ECH
	if config.EncryptedClientHelloConfigList != nil {
		return c.HandshakeContext(ctx)
	}
	// Iterate over extensions and check for utls.ALPNExtension
	hasALPNExtension := false
	for _, extension := range c.Extensions {
		if alpn, ok := extension.(*utls.ALPNExtension); ok {
			hasALPNExtension = true
			alpn.AlpnProtocols = []string{"http/1.1"}
			break
		}
	}
	if !hasALPNExtension { // Append extension if doesn't exists
		c.Extensions = append(c.Extensions, &utls.ALPNExtension{AlpnProtocols: []string{"http/1.1"}})
	}
	// Rebuild the client hello and do the handshake
	if err := c.BuildHandshakeState(); err != nil {
		return err
	}
	return c.HandshakeContext(ctx)
}

func (c *UConn) NegotiatedProtocol() string {
	state := c.ConnectionState()
	return state.NegotiatedProtocol
}

// VisionBuffers exposes uTLS read-ahead through the transport wrapper.
func (c *UConn) VisionBuffers() (*bytes.Reader, *bytes.Buffer) {
	input, inputOK := utils.TryAccessField[bytes.Reader](c.Conn, "input")
	rawInput, rawInputOK := utils.TryAccessField[bytes.Buffer](c.Conn, "rawInput")
	if !inputOK || !rawInputOK {
		return nil, nil
	}
	return input, rawInput
}

func UClient(c net.Conn, config *tls.Config, fingerprint *utls.ClientHelloID) net.Conn {
	utlsConn := utls.UClient(c, copyConfig(config), *fingerprint)
	return &UConn{UConn: utlsConn}
}

func GeneraticUClient(c net.Conn, config *tls.Config) *utls.UConn {
	return utls.UClient(c, copyConfig(config), utls.HelloChrome_Auto)
}

func copyConfig(c *tls.Config) *utls.Config {
	config := &utls.Config{
		Rand:                           c.Rand,
		RootCAs:                        c.RootCAs,
		ServerName:                     c.ServerName,
		InsecureSkipVerify:             c.InsecureSkipVerify,
		VerifyPeerCertificate:          c.VerifyPeerCertificate,
		KeyLogWriter:                   c.KeyLogWriter,
		EncryptedClientHelloConfigList: c.EncryptedClientHelloConfigList,
	}
	if config.EncryptedClientHelloConfigList != nil {
		config.NextProtos = c.NextProtos
	}
	return config
}

func init() {
	bigInt, _ := rand.Int(rand.Reader, big.NewInt(int64(len(ModernFingerprints))))
	stopAt := int(bigInt.Int64())
	i := 0
	for _, v := range ModernFingerprints {
		if i == stopAt {
			PresetFingerprints["random"] = v
			break
		}
		i++
	}
	weights := utls.DefaultWeights
	weights.TLSVersMax_Set_VersionTLS13 = 1
	weights.FirstKeyShare_Set_CurveP256 = 0
	randomized := utls.HelloRandomizedALPN
	randomized.Seed, _ = utls.NewPRNGSeed()
	randomized.Weights = &weights
	randomizednoalpn := utls.HelloRandomizedNoALPN
	randomizednoalpn.Seed, _ = utls.NewPRNGSeed()
	randomizednoalpn.Weights = &weights
	PresetFingerprints["randomized"] = &randomized
	PresetFingerprints["randomizednoalpn"] = &randomizednoalpn
}

func GetFingerprint(name string) (fingerprint *utls.ClientHelloID) {
	if name == "" {
		return &utls.HelloChrome_Auto
	}
	if fingerprint = PresetFingerprints[name]; fingerprint != nil {
		return
	}
	if fingerprint = ModernFingerprints[name]; fingerprint != nil {
		return
	}
	if fingerprint = OtherFingerprints[name]; fingerprint != nil {
		return
	}
	return
}

var PresetFingerprints = map[string]*utls.ClientHelloID{
	// Recommended preset options in GUI clients
	"chrome":           &utls.HelloChrome_Auto,
	"firefox":          &utls.HelloFirefox_Auto,
	"safari":           &utls.HelloSafari_Auto,
	"ios":              &utls.HelloIOS_Auto,
	"android":          &utls.HelloAndroid_11_OkHttp,
	"edge":             &utls.HelloEdge_Auto,
	"360":              &utls.Hello360_Auto,
	"qq":               &utls.HelloQQ_Auto,
	"random":           nil,
	"randomized":       nil,
	"randomizednoalpn": nil,
	"unsafe":           nil,
}

var ModernFingerprints = map[string]*utls.ClientHelloID{
	// One of these will be chosen as `random` at startup
	"hellofirefox_99":         &utls.HelloFirefox_99,
	"hellofirefox_102":        &utls.HelloFirefox_102,
	"hellofirefox_105":        &utls.HelloFirefox_105,
	"hellofirefox_120":        &utls.HelloFirefox_120,
	"hellochrome_83":          &utls.HelloChrome_83,
	"hellochrome_87":          &utls.HelloChrome_87,
	"hellochrome_96":          &utls.HelloChrome_96,
	"hellochrome_100":         &utls.HelloChrome_100,
	"hellochrome_102":         &utls.HelloChrome_102,
	"hellochrome_106_shuffle": &utls.HelloChrome_106_Shuffle,
	"hellochrome_120":         &utls.HelloChrome_120,
	"hellochrome_131":         &utls.HelloChrome_131,
	"helloios_13":             &utls.HelloIOS_13,
	"helloios_14":             &utls.HelloIOS_14,
	"helloedge_85":            &utls.HelloEdge_85,
	"helloedge_106":           &utls.HelloEdge_106,
	"hellosafari_16_0":        &utls.HelloSafari_16_0,
	"hello360_11_0":           &utls.Hello360_11_0,
	"helloqq_11_1":            &utls.HelloQQ_11_1,
}

var OtherFingerprints = map[string]*utls.ClientHelloID{
	// Golang, randomized, auto, and fingerprints that are too old
	"hellogolang":            &utls.HelloGolang,
	"hellorandomized":        &utls.HelloRandomized,
	"hellorandomizedalpn":    &utls.HelloRandomizedALPN,
	"hellorandomizednoalpn":  &utls.HelloRandomizedNoALPN,
	"hellofirefox_auto":      &utls.HelloFirefox_Auto,
	"hellofirefox_55":        &utls.HelloFirefox_55,
	"hellofirefox_56":        &utls.HelloFirefox_56,
	"hellofirefox_63":        &utls.HelloFirefox_63,
	"hellofirefox_65":        &utls.HelloFirefox_65,
	"hellochrome_auto":       &utls.HelloChrome_Auto,
	"hellochrome_58":         &utls.HelloChrome_58,
	"hellochrome_62":         &utls.HelloChrome_62,
	"hellochrome_70":         &utls.HelloChrome_70,
	"hellochrome_72":         &utls.HelloChrome_72,
	"helloios_auto":          &utls.HelloIOS_Auto,
	"helloios_11_1":          &utls.HelloIOS_11_1,
	"helloios_12_1":          &utls.HelloIOS_12_1,
	"helloandroid_11_okhttp": &utls.HelloAndroid_11_OkHttp,
	"helloedge_auto":         &utls.HelloEdge_Auto,
	"hellosafari_auto":       &utls.HelloSafari_Auto,
	"hello360_auto":          &utls.Hello360_Auto,
	"hello360_7_5":           &utls.Hello360_7_5,
	"helloqq_auto":           &utls.HelloQQ_Auto,

	// Chrome betas'
	"hellochrome_100_psk":              &utls.HelloChrome_100_PSK,
	"hellochrome_112_psk_shuf":         &utls.HelloChrome_112_PSK_Shuf,
	"hellochrome_114_padding_psk_shuf": &utls.HelloChrome_114_Padding_PSK_Shuf,
	"hellochrome_115_pq":               &utls.HelloChrome_115_PQ,
	"hellochrome_115_pq_psk":           &utls.HelloChrome_115_PQ_PSK,
	"hellochrome_120_pq":               &utls.HelloChrome_120_PQ,
}
