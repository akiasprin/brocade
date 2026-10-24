//go:build linux

package tcp

import (
	"bytes"
	"context"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
	"os"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
	"sync"
	"time"

	"github.com/xtls/xray-core/transport/internet"
	"golang.org/x/sys/unix"
)

const (
	handoffMaxMessage = 256 * 1024
	handoffTimeout    = 3 * time.Second
)

type handoffEndpoint struct {
	Address            string `json:"address"`
	BindAddress        string `json:"bind_address,omitempty"`
	Network            string `json:"network,omitempty"`
	Role               string `json:"role,omitempty"`
	Prepare            bool   `json:"prepare,omitempty"`
	DependsOn          string `json:"depends_on,omitempty"`
	FD                 int    `json:"fd"`
	SocketInode        uint64 `json:"socket_inode"`
	SocketCookie       uint64 `json:"socket_cookie"`
	Protocol           int    `json:"protocol"`
	SocketConfigSHA256 string `json:"socket_config_sha256"`
}

type handoffManifest struct {
	Version      int               `json:"version"`
	Scope        string            `json:"scope,omitempty"`
	GenerationID string            `json:"generation_id"`
	ControlPath  string            `json:"control_path"`
	Listeners    []handoffEndpoint `json:"listeners"`
}

type handoffSocketIdentity struct {
	Address      string `json:"address"`
	SocketInode  uint64 `json:"socket_inode"`
	SocketCookie uint64 `json:"socket_cookie"`
	Protocol     int    `json:"protocol"`
}

type handoffListenerStatus struct {
	handoffSocketIdentity
	BindAddress        string `json:"bind_address"`
	Network            string `json:"network"`
	Role               string `json:"role"`
	DependsOn          string `json:"depends_on,omitempty"`
	Accepted           uint64 `json:"accepted"`
	Active             uint64 `json:"active"`
	InAccept           bool   `json:"in_accept"`
	Closed             bool   `json:"closed"`
	SocketConfigSHA256 string `json:"socket_config_sha256"`
}

type handoffStatus struct {
	Scope               string                  `json:"scope"`
	RetirementSupported bool                    `json:"retirement_supported"`
	GenerationID        string                  `json:"generation_id"`
	PID                 int                     `json:"pid"`
	BootID              string                  `json:"boot_id"`
	PIDStartTicks       string                  `json:"pid_start_ticks"`
	Ready               bool                    `json:"ready"`
	Paused              bool                    `json:"paused"`
	PauseRequested      bool                    `json:"pause_requested"`
	Accepting           bool                    `json:"accepting"`
	Epoch               uint64                  `json:"epoch"`
	Intent              string                  `json:"intent"`
	Operation           string                  `json:"operation"`
	RetireReady         bool                    `json:"retire_ready"`
	DrainRequested      bool                    `json:"drain_requested"`
	Accepted            uint64                  `json:"accepted"`
	Active              uint64                  `json:"active"`
	PublicActive        uint64                  `json:"public_active"`
	CoverActive         uint64                  `json:"cover_active"`
	UDPDropped          bool                    `json:"udp_dropped"`
	UnsupportedReasons  []string                `json:"unsupported_reasons,omitempty"`
	Listeners           []handoffListenerStatus `json:"listeners"`
}

// Each mutation installs and executes one monotonic fencing intent under opMu.
// A delayed request from an older manager can never overwrite a newer pause.
type handoffRequest struct {
	Operation    string `json:"operation"`
	GenerationID string `json:"generation_id"`
	Epoch        uint64 `json:"epoch"`
	Intent       string `json:"intent"`
}

type handoffResponse struct {
	Error  string         `json:"error,omitempty"`
	Status *handoffStatus `json:"status,omitempty"`
}

type tcpHandoffBinding struct {
	owner              *tcpHandoff
	listener           *net.TCPListener
	identity           handoffSocketIdentity
	inAccept           bool
	closed             bool
	retired            bool
	runtimeRemoved     bool
	accepted           uint64
	active             uint64
	keepAlive          *net.KeepAliveConfig
	socketConfigSHA256 string
	endpoint           handoffEndpoint
}

type packetHandoffBinding struct {
	owner              *tcpHandoff
	conn               *net.UDPConn
	identity           handoffSocketIdentity
	endpoint           handoffEndpoint
	inAccept           bool
	closed             bool
	runtimeRemoved     bool
	accepted           uint64
	active             uint64
	acceptCancel       context.CancelFunc
	socketConfigSHA256 string
}

type tcpHandoff struct {
	opMu             sync.Mutex
	mu               sync.Mutex
	cond             *sync.Cond
	manifest         handoffManifest
	expected         map[string]handoffEndpoint
	bindings         map[string]*tcpHandoffBinding
	packets          map[string]*packetHandoffBinding
	unsupported      map[string]bool
	control          *net.UnixListener
	worker           sync.WaitGroup
	coreReady        bool
	ready            bool
	paused           bool
	coverPaused      bool
	drainRequested   bool
	udpDropRequested bool
	udpDropped       bool
	tcpDropped       bool
	closed           bool
	epoch            uint64
	intent           string
	operation        string
	bootID           string
	pidStartTicks    string
	packetRelease    func()
}

var handoffRegistry struct {
	sync.Mutex
	control *tcpHandoff
}

// InitHandoff reads the explicitly enabled, private manifest before Xray starts.
// Without XRAY_TCP_HANDOFF_MANIFEST it does nothing. Imported sockets retain
// their original kernel socket options; changing those options is not supported
// by this listener-admission mechanism.
func InitHandoff() error {
	path := os.Getenv("XRAY_TCP_HANDOFF_MANIFEST")
	if path == "" {
		return nil
	}
	handoffRegistry.Lock()
	defer handoffRegistry.Unlock()
	if handoffRegistry.control != nil {
		return fmt.Errorf("TCP handoff is already initialized")
	}
	manifest, err := readHandoffManifest(path)
	if err != nil {
		return err
	}
	c, err := newTCPHandoff(manifest)
	if err != nil {
		return err
	}
	handoffRegistry.control = c
	return nil
}

// MarkHandoffReady confirms every selected listener was registered. It does not
// open admission: an explicit, fenced resume request is still required.
func MarkHandoffReady() error {
	handoffRegistry.Lock()
	c := handoffRegistry.control
	handoffRegistry.Unlock()
	if c == nil {
		return nil
	}
	return c.markReady()
}

// CloseHandoff closes the private control socket and stops selected listeners
// from accepting. It never closes or terminates an already accepted connection.
// This is not a drain or a process-retirement operation.
func CloseHandoff() error {
	handoffRegistry.Lock()
	c := handoffRegistry.control
	handoffRegistry.control = nil
	handoffRegistry.Unlock()
	if c == nil {
		return nil
	}
	return c.close()
}

func decodeHandoffJSON(data []byte, target any) error {
	if len(data) > handoffMaxMessage {
		return fmt.Errorf("TCP handoff message is too large")
	}
	d := json.NewDecoder(bytes.NewReader(data))
	if err := d.Decode(target); err != nil {
		return fmt.Errorf("invalid TCP handoff JSON: %w", err)
	}
	if err := d.Decode(new(any)); err != io.EOF {
		return fmt.Errorf("TCP handoff JSON must contain exactly one value")
	}
	return nil
}

func validHandoffToken(token string) bool {
	return len(token) >= 16 && validHandoffIntent(token)
}

// Fencing intents are operation identities, not random generation identifiers.
// Small deployment/claim IDs legitimately produce labels such as config-2-1-seal.
// Same-UID authentication and exact epoch/intent/operation matching protect RPCs.
func validHandoffIntent(token string) bool {
	if len(token) == 0 || len(token) > 128 {
		return false
	}
	for _, ch := range token {
		if (ch < 'a' || ch > 'z') && (ch < 'A' || ch > 'Z') && (ch < '0' || ch > '9') && ch != '-' && ch != '_' {
			return false
		}
	}
	return true
}

// Refuse symlink components and require a private directory owned by this uid.
// Ancestors may be root-owned; only sticky directories may be world-writable.
func checkHandoffParent(path string) error {
	if !filepath.IsAbs(path) || filepath.Clean(path) != path || strings.ContainsRune(path, '\x00') {
		return fmt.Errorf("TCP handoff path must be an absolute clean filesystem path")
	}
	parent := filepath.Dir(path)
	for dir := parent; ; dir = filepath.Dir(dir) {
		var st unix.Stat_t
		if err := unix.Lstat(dir, &st); err != nil {
			return fmt.Errorf("inspect TCP handoff directory: %w", err)
		}
		if st.Mode&unix.S_IFMT != unix.S_IFDIR {
			return fmt.Errorf("TCP handoff ancestor is not a directory")
		}
		if dir == parent {
			if st.Uid != uint32(os.Geteuid()) || st.Mode&0077 != 0 {
				return fmt.Errorf("TCP handoff parent must be owned by this uid without group or other access")
			}
		} else if st.Mode&0022 != 0 && st.Mode&unix.S_ISVTX == 0 {
			return fmt.Errorf("TCP handoff ancestor %s mode %#o is writable without the sticky bit", dir, st.Mode)
		}
		if dir == "/" {
			break
		}
	}
	return nil
}

func readHandoffManifest(path string) (handoffManifest, error) {
	var manifest handoffManifest
	if err := checkHandoffParent(path); err != nil {
		return manifest, err
	}
	fd, err := unix.Open(path, unix.O_RDONLY|unix.O_CLOEXEC|unix.O_NOFOLLOW|unix.O_NONBLOCK, 0)
	if err != nil {
		return manifest, fmt.Errorf("open TCP handoff manifest: %w", err)
	}
	f := os.NewFile(uintptr(fd), "tcp-handoff-manifest")
	defer f.Close()
	var st unix.Stat_t
	if err := unix.Fstat(fd, &st); err != nil {
		return manifest, err
	}
	if st.Mode&unix.S_IFMT != unix.S_IFREG || st.Mode&0077 != 0 || st.Uid != uint32(os.Geteuid()) || st.Nlink != 1 {
		return manifest, fmt.Errorf("TCP handoff manifest must be a singly-linked private regular file owned by this uid")
	}
	data, err := io.ReadAll(io.LimitReader(f, handoffMaxMessage+1))
	if err != nil {
		return manifest, err
	}
	if err := decodeHandoffJSON(data, &manifest); err != nil {
		return manifest, err
	}
	return manifest, validateHandoffManifest(manifest)
}

func validateHandoffManifest(manifest handoffManifest) error {
	if (manifest.Version != 1 && manifest.Version != 2) || !validHandoffToken(manifest.GenerationID) || len(manifest.Listeners) < 1 || len(manifest.Listeners) > 512 {
		return fmt.Errorf("invalid TCP handoff manifest version, generation or listener count")
	}
	if manifest.Version == 1 && manifest.Scope != "" {
		return fmt.Errorf("TCP handoff manifest v1 must not declare a retirement scope")
	}
	if manifest.Version == 2 && manifest.Scope != "whole-generation" {
		return fmt.Errorf("handoff manifest v2 requires whole-generation scope")
	}
	if len(manifest.ControlPath) >= 108 {
		return fmt.Errorf("TCP handoff control path is too long")
	}
	if err := checkHandoffParent(manifest.ControlPath); err != nil {
		return err
	}
	addresses, fds := make(map[string]bool), make(map[int]bool)
	for _, rawEndpoint := range manifest.Listeners {
		endpoint := normalizedHandoffEndpoint(manifest.Version, rawEndpoint)
		host, port, err := net.SplitHostPort(endpoint.Address)
		p, parseErr := strconv.Atoi(port)
		ip := net.ParseIP(host)
		canonical := ""
		if endpoint.Network == "tcp" {
			canonical = (&net.TCPAddr{IP: ip, Port: p}).String()
		} else if endpoint.Network == "udp" {
			canonical = (&net.UDPAddr{IP: ip, Port: p}).String()
		}
		key := handoffEndpointKey(endpoint.Network, endpoint.Address)
		if err != nil || parseErr != nil || ip == nil || p < 1 || p > 65535 || canonical != endpoint.Address || addresses[key] {
			return fmt.Errorf("invalid or duplicate handoff listener address")
		}
		addresses[key] = true
		bindHost, bindPort, bindErr := net.SplitHostPort(endpoint.BindAddress)
		bindNumber, bindParseErr := strconv.Atoi(bindPort)
		bindIP := net.ParseIP(bindHost)
		bindCanonical := ""
		if endpoint.Network == "tcp" {
			bindCanonical = (&net.TCPAddr{IP: bindIP, Port: bindNumber}).String()
		} else if endpoint.Network == "udp" {
			bindCanonical = (&net.UDPAddr{IP: bindIP, Port: bindNumber}).String()
		}
		minimumBindPort := 1
		if endpoint.Network == "udp" {
			// Port zero lets the kernel select a collision-free generation-private port. The
			// actual address is returned in status before the release manager installs NAT.
			minimumBindPort = 0
		}
		if bindErr != nil || bindParseErr != nil || bindIP == nil || bindNumber < minimumBindPort || bindNumber > 65535 || bindCanonical != endpoint.BindAddress {
			return fmt.Errorf("invalid handoff bind address")
		}
		if endpoint.Role != "public" && endpoint.Role != "cover" && endpoint.Role != "control" {
			return fmt.Errorf("invalid handoff listener role")
		}
		if endpoint.Role == "cover" && endpoint.Network != "tcp" {
			return fmt.Errorf("handoff cover listeners must use TCP")
		}
		if endpoint.Role == "control" {
			if endpoint.Network != "tcp" || !ip.IsLoopback() {
				return fmt.Errorf("handoff control listeners must be loopback TCP")
			}
		} else if endpoint.Prepare {
			return fmt.Errorf("only a handoff control listener may accept during preparation")
		}
		if endpoint.Network == "tcp" && endpoint.Protocol != 0 && endpoint.Protocol != unix.IPPROTO_TCP && endpoint.Protocol != unix.IPPROTO_MPTCP {
			return fmt.Errorf("TCP handoff requires an explicit TCP or MPTCP protocol")
		}
		if endpoint.Network == "udp" && endpoint.Protocol != unix.IPPROTO_UDP {
			return fmt.Errorf("UDP handoff requires the UDP protocol")
		}
		if endpoint.Network == "udp" && endpoint.FD != 0 {
			return fmt.Errorf("UDP handoff uses a generation-specific bind address, not a shared FD")
		}
		if endpoint.FD == 0 {
			if endpoint.Network == "udp" && endpoint.Protocol == 0 {
				return fmt.Errorf("UDP handoff requires an explicit protocol")
			}
			if endpoint.SocketInode != 0 || endpoint.SocketCookie != 0 || endpoint.SocketConfigSHA256 != "" {
				return fmt.Errorf("a newly bound handoff listener must not claim an existing identity")
			}
			continue
		}
		if endpoint.Protocol == 0 {
			return fmt.Errorf("inherited TCP handoff requires an explicit protocol")
		}
		if endpoint.FD < 3 || fds[endpoint.FD] || endpoint.SocketInode == 0 || endpoint.SocketCookie == 0 {
			return fmt.Errorf("invalid inherited TCP handoff FD or missing identity")
		}
		decoded, err := hex.DecodeString(endpoint.SocketConfigSHA256)
		if err != nil || len(decoded) != 32 || hex.EncodeToString(decoded) != endpoint.SocketConfigSHA256 {
			return fmt.Errorf("inherited TCP handoff requires a lowercase SHA256 socket configuration digest")
		}
		fds[endpoint.FD] = true
		identity, err := inspectHandoffFD(endpoint.FD)
		if err != nil {
			return err
		}
		if !matchesHandoffEndpoint(identity, endpoint) {
			return fmt.Errorf("inherited TCP handoff socket identity mismatch")
		}
	}
	return nil
}

func normalizedHandoffEndpoint(version int, endpoint handoffEndpoint) handoffEndpoint {
	if endpoint.Network == "" {
		endpoint.Network = "tcp"
	}
	if endpoint.BindAddress == "" {
		endpoint.BindAddress = endpoint.Address
	}
	if endpoint.Role == "" {
		endpoint.Role = "public"
	}
	if version == 1 {
		endpoint.Network = "tcp"
		endpoint.BindAddress = endpoint.Address
		endpoint.Role = "public"
		endpoint.DependsOn = ""
	}
	return endpoint
}

func handoffEndpointKey(network, address string) string {
	return network + "\x00" + address
}

func newTCPHandoff(manifest handoffManifest) (*tcpHandoff, error) {
	if err := validateHandoffManifest(manifest); err != nil {
		return nil, err
	}
	boot, err := os.ReadFile("/proc/sys/kernel/random/boot_id")
	if err != nil {
		return nil, err
	}
	stat, err := os.ReadFile("/proc/self/stat")
	if err != nil {
		return nil, err
	}
	fields := strings.Fields(string(stat)[strings.LastIndex(string(stat), ")")+1:])
	if len(fields) < 20 || strings.TrimSpace(string(boot)) == "" {
		return nil, fmt.Errorf("process identity is unavailable for TCP handoff")
	}
	c := &tcpHandoff{manifest: manifest, expected: make(map[string]handoffEndpoint), bindings: make(map[string]*tcpHandoffBinding), packets: make(map[string]*packetHandoffBinding), unsupported: make(map[string]bool), paused: true, coverPaused: true, bootID: strings.TrimSpace(string(boot)), pidStartTicks: fields[19]}
	c.cond = sync.NewCond(&c.mu)
	for _, rawEndpoint := range manifest.Listeners {
		endpoint := normalizedHandoffEndpoint(manifest.Version, rawEndpoint)
		c.expected[handoffEndpointKey(endpoint.Network, endpoint.Address)] = endpoint
	}
	// Never unlink a pre-existing control socket or file. The manager must use a
	// generation-specific path; stale clients are rejected by generation_id too.
	control, err := net.ListenUnix("unixpacket", &net.UnixAddr{Name: manifest.ControlPath, Net: "unixpacket"})
	if err != nil {
		return nil, fmt.Errorf("create TCP handoff control socket: %w", err)
	}
	if err := os.Chmod(manifest.ControlPath, 0600); err != nil {
		control.Close()
		return nil, err
	}
	c.control = control
	if manifest.Version >= 2 {
		packetRelease, err := internet.RegisterPacketHandoffController(c.openPacket)
		if err != nil {
			control.Close()
			_ = os.Remove(manifest.ControlPath)
			return nil, err
		}
		c.packetRelease = packetRelease
	}
	c.worker.Add(1)
	go c.serve()
	return c, nil
}

func openHandoffListener(address string, unsupported bool, config *internet.SocketConfig, bind func() (net.Listener, error)) (net.Listener, handoffGate, error) {
	handoffRegistry.Lock()
	c := handoffRegistry.control
	handoffRegistry.Unlock()
	if c == nil {
		l, err := bind()
		return l, nil, err
	}
	return c.open(address, unsupported, config, bind)
}

func (c *tcpHandoff) open(address string, unsupported bool, config *internet.SocketConfig, bind func() (net.Listener, error)) (net.Listener, handoffGate, error) {
	c.mu.Lock()
	defer c.mu.Unlock()
	key := handoffEndpointKey("tcp", address)
	endpoint, found := c.expected[key]
	runtimeAddition := !found && !unsupported && c.runtimeMutationAllowedLocked()
	if runtimeAddition {
		if len(c.expected) >= 512 {
			return nil, nil, fmt.Errorf("runtime handoff listener limit reached")
		}
		endpoint = handoffEndpoint{Address: address, BindAddress: address, Network: "tcp", Role: "public"}
		found = true
	}
	if !found {
		if c.manifest.Version >= 2 {
			c.unsupported["unregistered_tcp_listener"] = true
		}
		l, err := bind()
		return l, nil, err
	}
	if c.closed || (c.ready && !runtimeAddition) || c.bindings[key] != nil || unsupported {
		return nil, nil, fmt.Errorf("TCP handoff listener is closed, duplicate or uses unsupported wrapping")
	}
	// MPTCP is a preference, not a requirement: Go can fall back to TCP when
	// the kernel cannot honor the requested keepalive policy. The manifest
	// still pins the actual protocol and FD identity, never a guessed protocol.
	socketConfigSHA256, err := handoffSocketConfigDigest(config)
	if err != nil {
		return nil, nil, fmt.Errorf("hash TCP handoff socket configuration: %w", err)
	}
	if endpoint.FD != 0 && endpoint.SocketConfigSHA256 != socketConfigSHA256 {
		return nil, nil, fmt.Errorf("TCP handoff socket configuration differs from the inherited listener")
	}
	var listener net.Listener
	if endpoint.FD == 0 {
		listener, err = bind()
	} else {
		identity, inspectErr := inspectHandoffFD(endpoint.FD)
		if inspectErr != nil || !matchesHandoffEndpoint(identity, endpoint) {
			return nil, nil, fmt.Errorf("TCP handoff inherited listener identity changed before import")
		}
		f := os.NewFile(uintptr(endpoint.FD), "tcp-handoff-listener")
		listener, err = net.FileListener(f)
		f.Close()
	}
	if err != nil {
		return nil, nil, fmt.Errorf("open selected TCP handoff listener: %w", err)
	}
	tcpListener, ok := listener.(*net.TCPListener)
	if !ok {
		listener.Close()
		return nil, nil, fmt.Errorf("TCP handoff requires an unwrapped TCP listener")
	}
	if err := internet.ValidateTCPListenerKeepAlive(tcpListener, config); err != nil {
		listener.Close()
		return nil, nil, fmt.Errorf("validate TCP handoff keepalive: %w", err)
	}
	identity, err := inspectHandoffListener(tcpListener)
	if err != nil {
		listener.Close()
		return nil, nil, fmt.Errorf("inspect opened TCP handoff listener: %w", err)
	}
	addressMatches := identity.Address == endpoint.BindAddress
	if endpoint.FD == 0 {
		addressMatches = handoffListenerMatchesRequestedAddress(tcpListener, identity.Address, address)
	}
	if !addressMatches || (endpoint.Protocol != 0 && identity.Protocol != endpoint.Protocol) || (endpoint.FD != 0 && !matchesHandoffEndpoint(identity, endpoint)) {
		listener.Close()
		return nil, nil, fmt.Errorf("opened TCP handoff listener identity does not match its manifest: actual address=%s protocol=%d expected address=%s bind_address=%s protocol=%d inherited=%t", identity.Address, identity.Protocol, address, endpoint.BindAddress, endpoint.Protocol, endpoint.FD != 0)
	}
	if endpoint.Protocol == 0 {
		endpoint.Protocol = identity.Protocol
	}
	if endpoint.FD == 0 {
		// Go may satisfy an explicit IPv4 wildcard request with one dual-stack IPv6
		// listener. Keep Address as Xray's logical configuration key and retain the
		// actual kernel bind separately for exact FD identity checks during handoff.
		endpoint.BindAddress = identity.Address
	}
	b := &tcpHandoffBinding{owner: c, listener: tcpListener, identity: identity, socketConfigSHA256: socketConfigSHA256, endpoint: endpoint}
	if endpoint.FD != 0 {
		keepAlive, err := handoffKeepAlive(tcpListener)
		if err != nil {
			listener.Close()
			return nil, nil, err
		}
		b.keepAlive = &keepAlive
	}
	c.bindings[key] = b
	if runtimeAddition {
		c.expected[key] = endpoint
	}
	return listener, b, nil
}

// OpenHandoffPacketListener binds the public UDP address declared by a v2 manifest. Each
// generation owns a separate SO_REUSEPORT socket; sharing one UDP FD between processes is
// intentionally forbidden because their packet loops would race on one receive queue. UDP is
// reconnect-only: the manager resumes the candidate before closing the old generation's socket.
func (c *tcpHandoff) openPacket(logicalAddress string, config *internet.SocketConfig, bind func(string) (net.PacketConn, error)) (net.PacketConn, internet.PacketHandoffGate, error) {
	c.mu.Lock()
	defer c.mu.Unlock()
	key := handoffEndpointKey("udp", logicalAddress)
	endpoint, found := c.expected[key]
	runtimeAddition := !found && c.runtimeMutationAllowedLocked()
	if runtimeAddition {
		if len(c.expected) >= 512 {
			return nil, nil, fmt.Errorf("runtime handoff listener limit reached")
		}
		endpoint = handoffEndpoint{Address: logicalAddress, BindAddress: logicalAddress, Network: "udp", Role: "public", Protocol: unix.IPPROTO_UDP}
		found = true
	}
	if !found {
		if c.manifest.Version >= 2 {
			c.unsupported["unregistered_udp_listener"] = true
		}
		conn, err := bind(logicalAddress)
		return conn, nil, err
	}
	if c.closed || (c.ready && !runtimeAddition) || c.packets[key] != nil {
		return nil, nil, fmt.Errorf("UDP handoff listener is closed or duplicate")
	}
	digest, err := handoffSocketConfigDigest(config)
	if err != nil {
		return nil, nil, fmt.Errorf("hash UDP handoff socket configuration: %w", err)
	}
	conn, err := bind(endpoint.BindAddress)
	if err != nil {
		return nil, nil, fmt.Errorf("open generation-specific UDP handoff listener: %w", err)
	}
	udpConn, ok := conn.(*net.UDPConn)
	if !ok {
		conn.Close()
		return nil, nil, fmt.Errorf("UDP handoff requires an unwrapped UDP socket")
	}
	identity, err := inspectHandoffPacketConn(udpConn)
	if err != nil || identity.Protocol != unix.IPPROTO_UDP || !handoffPacketMatchesRequestedAddress(udpConn, identity.Address, endpoint.BindAddress) {
		conn.Close()
		return nil, nil, fmt.Errorf("opened UDP handoff listener does not match its manifest: actual address=%s expected bind_address=%s", identity.Address, endpoint.BindAddress)
	}
	endpoint.BindAddress = identity.Address
	// Status uses the configured public address as the stable endpoint identity; BindAddress keeps
	// the exact kernel spelling (for example a dual-stack wildcard backing an IPv4 wildcard).
	identity.Address = endpoint.Address
	b := &packetHandoffBinding{owner: c, conn: udpConn, identity: identity, endpoint: endpoint, socketConfigSHA256: digest}
	c.packets[key] = b
	if runtimeAddition {
		c.expected[key] = endpoint
	}
	return conn, b, nil
}

// The Agent serializes live forwarding changes with generation handoff. Once a whole
// generation is serving, its public listeners may be revoked and recreated through gRPC.
// Preparation and pause/drain keep the frozen listener contract and fail closed.
func (c *tcpHandoff) runtimeMutationAllowedLocked() bool {
	return c.manifest.Version >= 2 && c.manifest.Scope == "whole-generation" && c.ready &&
		!c.closed && !c.paused && !c.coverPaused && !c.drainRequested
}

// A removed listener remains in the drain proof until both Accept and every admitted
// connection finish. Keeping it in the bounded listener map also prevents premature reuse.
func (b *tcpHandoffBinding) pruneRuntimeRemovedLocked() {
	c := b.owner
	key := handoffEndpointKey("tcp", b.endpoint.Address)
	if b.runtimeRemoved && !b.inAccept && b.active == 0 && c.bindings[key] == b {
		delete(c.bindings, key)
		delete(c.expected, key)
	}
}

func (b *packetHandoffBinding) pruneRuntimeRemovedLocked() {
	c := b.owner
	key := handoffEndpointKey("udp", b.endpoint.Address)
	if b.runtimeRemoved && !b.inAccept && b.active == 0 && c.packets[key] == b {
		delete(c.packets, key)
		delete(c.expected, key)
	}
}

func (c *tcpHandoff) markReady() error {
	c.mu.Lock()
	if c.closed {
		c.mu.Unlock()
		return fmt.Errorf("handoff listener group is closed")
	}
	c.coreReady = true
	// A v2 generation always stays in preparation until its manager explicitly seals it. Even a
	// fully static config may need grants restored through the loopback API before public
	// admission opens; auto-sealing here would pause that only preparation channel.
	if c.manifest.Version >= 2 {
		c.cond.Broadcast()
		c.mu.Unlock()
		return nil
	}
	if len(c.bindings)+len(c.packets) != len(c.expected) {
		c.mu.Unlock()
		return fmt.Errorf("handoff listener group is incomplete or closed")
	}
	err := c.sealLocked(true)
	c.mu.Unlock()
	if err != nil {
		return err
	}
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()
	return c.pause(ctx)
}

func (c *tcpHandoff) seal() error {
	c.mu.Lock()
	err := c.sealLocked(false)
	c.mu.Unlock()
	if err != nil {
		return err
	}
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()
	return c.pause(ctx)
}

func (c *tcpHandoff) sealLocked(allowUnsupported bool) error {
	if c.closed || !c.coreReady || len(c.bindings)+len(c.packets) != len(c.expected) || (!allowUnsupported && len(c.unsupported) != 0) {
		return fmt.Errorf("handoff listener group is incomplete, unsupported or closed")
	}
	for _, b := range c.bindings {
		if b.closed {
			return fmt.Errorf("selected TCP handoff listener is closed")
		}
	}
	for _, b := range c.packets {
		if b.closed {
			return fmt.Errorf("UDP handoff listener is closed")
		}
	}
	c.ready = true
	c.cond.Broadcast()
	return nil
}

func (c *tcpHandoff) status() handoffStatus {
	c.mu.Lock()
	defer c.mu.Unlock()
	pauseRequested := c.paused || !c.ready || c.closed
	retirementSupported := c.manifest.Version >= 2 && c.manifest.Scope == "whole-generation" && c.ready && !c.closed && len(c.unsupported) == 0
	scope := "selected_tcp_listeners"
	if c.manifest.Version >= 2 {
		scope = c.manifest.Scope
	}
	s := handoffStatus{Scope: scope, RetirementSupported: retirementSupported, GenerationID: c.manifest.GenerationID, PID: os.Getpid(), BootID: c.bootID, PIDStartTicks: c.pidStartTicks, Ready: c.ready && !c.closed, PauseRequested: pauseRequested, Paused: pauseRequested, Accepting: !pauseRequested, Epoch: c.epoch, Intent: c.intent, Operation: c.operation, DrainRequested: c.drainRequested, UDPDropped: c.udpDropped, Listeners: make([]handoffListenerStatus, 0, len(c.bindings)+len(c.packets))}
	for _, b := range c.bindings {
		// A requested pause is not a completed barrier while Accept can still
		// return a connection. Report that possibility even after a timeout.
		if b.endpoint.Role == "public" && b.inAccept {
			s.Paused = false
			s.Accepting = true
		}
		switch b.endpoint.Role {
		case "cover":
			s.CoverActive += b.active
			s.Active += b.active
			s.Accepted += b.accepted
		case "public":
			s.PublicActive += b.active
			s.Active += b.active
			s.Accepted += b.accepted
		}
		reportedIdentity := b.identity
		reportedIdentity.Address = b.endpoint.Address
		s.Listeners = append(s.Listeners, handoffListenerStatus{handoffSocketIdentity: reportedIdentity, BindAddress: b.endpoint.BindAddress, Network: "tcp", Role: b.endpoint.Role, DependsOn: b.endpoint.DependsOn, Accepted: b.accepted, Active: b.active, InAccept: b.inAccept, Closed: b.closed, SocketConfigSHA256: b.socketConfigSHA256})
	}
	for _, b := range c.packets {
		if !c.udpDropped && b.endpoint.Role == "public" && b.inAccept {
			s.Paused = false
			s.Accepting = true
		}
		if !c.udpDropped {
			switch b.endpoint.Role {
			case "cover":
				s.CoverActive += b.active
				s.Active += b.active
				s.Accepted += b.accepted
			case "public":
				s.PublicActive += b.active
				s.Active += b.active
				s.Accepted += b.accepted
			}
		}
		s.Listeners = append(s.Listeners, handoffListenerStatus{handoffSocketIdentity: b.identity, BindAddress: b.endpoint.BindAddress, Network: "udp", Role: b.endpoint.Role, DependsOn: b.endpoint.DependsOn, Accepted: b.accepted, Active: b.active, InAccept: b.inAccept, Closed: b.closed, SocketConfigSHA256: b.socketConfigSHA256})
	}
	for reason := range c.unsupported {
		s.UnsupportedReasons = append(s.UnsupportedReasons, reason)
	}
	sort.Strings(s.UnsupportedReasons)
	s.RetireReady = retirementSupported && c.drainRequested && c.paused && c.coverPaused && !s.Accepting && s.PublicActive == 0 && s.CoverActive == 0
	sort.Slice(s.Listeners, func(i, j int) bool {
		if s.Listeners[i].Network != s.Listeners[j].Network {
			return s.Listeners[i].Network < s.Listeners[j].Network
		}
		return s.Listeners[i].Address < s.Listeners[j].Address
	})
	return s
}

func (b *tcpHandoffBinding) beforeAccept() bool {
	c := b.owner
	c.mu.Lock()
	defer c.mu.Unlock()
	for !c.closed && !b.closed && !b.preparationAdmissionLocked() && (!c.ready || (b.endpoint.Role == "cover" && c.coverPaused) || (b.endpoint.Role != "cover" && c.paused)) {
		c.cond.Wait()
	}
	if c.closed || b.closed {
		return false
	}
	b.inAccept = true
	return true
}

// The only listener admitted before a v2 group is sealed is the loopback API explicitly marked
// by the private manifest. It lets the Agent recreate dynamic inbounds and grants while every
// public and cover listener remains paused.
func (b *tcpHandoffBinding) preparationAdmissionLocked() bool {
	c := b.owner
	return c.coreReady && !c.ready && b.endpoint.Role == "control" && b.endpoint.Prepare
}

func (b *tcpHandoffBinding) afterAccept(accepted bool) {
	c := b.owner
	c.mu.Lock()
	b.inAccept = false
	if accepted {
		b.accepted++
		b.active++
	}
	b.pruneRuntimeRemovedLocked()
	c.cond.Broadcast()
	c.mu.Unlock()
}

func (b *tcpHandoffBinding) trackConnection(conn net.Conn) {
	if previous, loaded := handoffConnections.LoadOrStore(conn, handoffGate(b)); loaded {
		// A connection may only own one lifetime pin. Keep the original registration and release
		// the duplicate pin so a caller bug cannot permanently block retirement accounting.
		_ = previous
		b.finishConnection()
	}
}

func (b *tcpHandoffBinding) finishConnection() {
	c := b.owner
	c.mu.Lock()
	if b.active > 0 {
		b.active--
	}
	b.pruneRuntimeRemovedLocked()
	c.cond.Broadcast()
	c.mu.Unlock()
}

func (b *tcpHandoffBinding) prepareConnection(conn net.Conn) error {
	if b.keepAlive == nil {
		return nil
	}
	tcpConn, ok := conn.(*net.TCPConn)
	if !ok {
		return fmt.Errorf("imported TCP handoff listener returned a non-TCP connection")
	}
	// FileListener has no ListenConfig and Go enables its own default keepalive
	// on Accept. Restore the inherited listener's settings without wrapping the
	// connection, so TCP half-close, kTLS and raw splice keep their concrete type.
	return tcpConn.SetKeepAliveConfig(*b.keepAlive)
}

func handoffKeepAlive(listener *net.TCPListener) (net.KeepAliveConfig, error) {
	var config net.KeepAliveConfig
	raw, err := listener.SyscallConn()
	if err != nil {
		return config, err
	}
	var failure error
	err = raw.Control(func(fd uintptr) {
		config, failure = handoffKeepAliveFD(int(fd), unix.GetsockoptInt)
	})
	if err != nil {
		return config, err
	}
	return config, failure
}

func handoffKeepAliveFD(fd int, get func(int, int, int) (int, error)) (net.KeepAliveConfig, error) {
	var config net.KeepAliveConfig
	enabled, err := get(fd, unix.SOL_SOCKET, unix.SO_KEEPALIVE)
	if err != nil {
		return config, err
	}
	var values [3]int
	for index, option := range []int{unix.TCP_KEEPIDLE, unix.TCP_KEEPINTVL, unix.TCP_KEEPCNT} {
		values[index], err = get(fd, unix.IPPROTO_TCP, option)
		if err != nil {
			protocol, protocolErr := get(fd, unix.SOL_SOCKET, unix.SO_PROTOCOL)
			if errors.Is(err, unix.EOPNOTSUPP) && protocolErr == nil && protocol == unix.IPPROTO_MPTCP && enabled == 0 {
				// Early MPTCP kernels implement SO_KEEPALIVE but neither read nor write
				// TCP_KEEP*. Restore the verified disabled state without asking Go to
				// write unsupported timers. Enabled keepalive still requires exact values.
				return net.KeepAliveConfig{Idle: -1, Interval: -1, Count: -1}, nil
			}
			return config, err
		}
	}
	return net.KeepAliveConfig{Enable: enabled != 0, Idle: time.Duration(values[0]) * time.Second, Interval: time.Duration(values[1]) * time.Second, Count: values[2]}, nil
}

func (b *tcpHandoffBinding) close() {
	c := b.owner
	c.mu.Lock()
	b.closed = true
	if b.runtimeRemoved || (!b.retired && b.endpoint.Role == "public" && c.runtimeMutationAllowedLocked()) {
		b.runtimeRemoved = true
		b.pruneRuntimeRemovedLocked()
		c.cond.Broadcast()
		c.mu.Unlock()
		return
	}
	// HTTP/gRPC servers close their listener again when Accept exits. An explicitly retired
	// admission socket is not an unexpected missing listener and must retain its drain proof.
	if !b.retired {
		c.ready = false
	}
	c.paused = true
	c.cond.Broadcast()
	c.mu.Unlock()
}

func (b *packetHandoffBinding) BeforeAccept(parent context.Context) (context.Context, bool) {
	c := b.owner
	c.mu.Lock()
	defer c.mu.Unlock()
	for !c.closed && !b.closed && (!c.ready || (b.endpoint.Role == "cover" && c.coverPaused) || (b.endpoint.Role != "cover" && c.paused)) {
		c.cond.Wait()
	}
	if c.closed || b.closed {
		return nil, false
	}
	ctx, cancel := context.WithCancel(parent)
	b.acceptCancel = cancel
	b.inAccept = true
	return ctx, true
}

func (b *packetHandoffBinding) AfterAccept(accepted bool) {
	c := b.owner
	c.mu.Lock()
	if b.acceptCancel != nil {
		b.acceptCancel()
		b.acceptCancel = nil
	}
	b.inAccept = false
	if accepted {
		b.accepted++
		b.active++
	}
	b.pruneRuntimeRemovedLocked()
	c.cond.Broadcast()
	c.mu.Unlock()
}

func (b *packetHandoffBinding) FinishConnection() {
	c := b.owner
	c.mu.Lock()
	if b.active > 0 {
		b.active--
	}
	b.pruneRuntimeRemovedLocked()
	c.cond.Broadcast()
	c.mu.Unlock()
}

func (b *packetHandoffBinding) StartConnection() bool {
	c := b.owner
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.closed || b.closed || !c.ready || (b.endpoint.Role == "cover" && c.coverPaused) || (b.endpoint.Role != "cover" && c.paused) {
		return false
	}
	b.accepted++
	b.active++
	return true
}

func (b *packetHandoffBinding) Close() {
	c := b.owner
	c.mu.Lock()
	if b.closed && c.udpDropRequested {
		c.mu.Unlock()
		return
	}
	b.closed = true
	if b.acceptCancel != nil {
		b.acceptCancel()
		b.acceptCancel = nil
	}
	if b.runtimeRemoved || (b.endpoint.Role == "public" && c.runtimeMutationAllowedLocked()) {
		b.runtimeRemoved = true
		b.pruneRuntimeRemovedLocked()
		c.cond.Broadcast()
		c.mu.Unlock()
		return
	}
	c.ready = false
	c.paused = true
	c.coverPaused = true
	c.cond.Broadcast()
	c.mu.Unlock()
}

func (c *tcpHandoff) dropUDP() error {
	c.mu.Lock()
	if c.closed || !c.ready || !c.paused {
		c.mu.Unlock()
		return fmt.Errorf("UDP reset requires a ready paused generation")
	}
	if c.udpDropped {
		c.mu.Unlock()
		return nil
	}
	connections := make([]*net.UDPConn, 0, len(c.packets))
	c.udpDropRequested = true
	for _, b := range c.packets {
		b.closed = true
		if b.acceptCancel != nil {
			b.acceptCancel()
			b.acceptCancel = nil
		}
		connections = append(connections, b.conn)
	}
	c.cond.Broadcast()
	c.mu.Unlock()

	var failure error
	for _, conn := range connections {
		if err := conn.Close(); err != nil && !errors.Is(err, net.ErrClosed) {
			failure = errors.Join(failure, err)
		}
	}
	if failure == nil {
		c.mu.Lock()
		c.udpDropped = true
		c.cond.Broadcast()
		c.mu.Unlock()
	}
	return failure
}

func (c *tcpHandoff) pause(ctx context.Context) error {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.closed {
		return fmt.Errorf("TCP handoff control is closed")
	}
	c.paused = true
	for _, b := range c.bindings {
		if b.endpoint.Role == "cover" || b.closed {
			continue
		}
		if err := b.listener.SetDeadline(time.Now()); err != nil {
			return fmt.Errorf("interrupt TCP listener accept: %w", err)
		}
	}
	for _, b := range c.packets {
		if b.endpoint.Role != "cover" && b.acceptCancel != nil {
			b.acceptCancel()
		}
	}
	stop := context.AfterFunc(ctx, func() { c.mu.Lock(); c.cond.Broadcast(); c.mu.Unlock() })
	defer stop()
	for {
		pending := false
		for _, b := range c.bindings {
			pending = pending || (b.endpoint.Role != "cover" && b.inAccept)
		}
		for _, b := range c.packets {
			pending = pending || (b.endpoint.Role != "cover" && b.inAccept)
		}
		if !pending {
			return nil
		}
		if err := ctx.Err(); err != nil {
			return err
		}
		c.cond.Wait()
	}
}

// dropTCP closes only this generation's public/control listening descriptors. Shared descriptors
// remain alive in the candidate; accepted TCP sockets and old REALITY cover work are untouched.
// This is irreversible and is called only after a complete candidate has taken admission.
func (c *tcpHandoff) dropTCP() error {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.tcpDropped {
		return nil
	}
	if c.closed || !c.ready || c.manifest.Version < 2 || !c.paused {
		return fmt.Errorf("TCP admission retirement requires a ready paused generation")
	}
	for _, b := range c.bindings {
		if b.endpoint.Role != "cover" && b.inAccept {
			return fmt.Errorf("TCP admission retirement requires the completed pause barrier")
		}
	}
	var failure error
	for _, b := range c.bindings {
		if b.endpoint.Role == "cover" || b.closed {
			continue
		}
		if err := b.listener.Close(); err != nil && !errors.Is(err, net.ErrClosed) {
			failure = errors.Join(failure, err)
			continue
		}
		b.closed = true
		b.retired = true
	}
	c.cond.Broadcast()
	if failure == nil {
		c.tcpDropped = true
	}
	return failure
}

func (c *tcpHandoff) drain(ctx context.Context) error {
	c.mu.Lock()
	defer c.mu.Unlock()
	udpReady := len(c.packets) == 0 || c.udpDropped
	if c.closed || !c.ready || c.manifest.Version < 2 || c.manifest.Scope != "whole-generation" || len(c.unsupported) != 0 || !c.paused || !udpReady {
		return fmt.Errorf("generation is not eligible for complete retirement drain")
	}
	for _, b := range c.bindings {
		if b.endpoint.Role == "public" && (b.inAccept || b.active != 0) {
			return fmt.Errorf("public TCP connections have not drained")
		}
	}
	c.drainRequested = true
	c.coverPaused = true
	for _, b := range c.bindings {
		if b.endpoint.Role == "cover" {
			if err := b.listener.SetDeadline(time.Now()); err != nil {
				return fmt.Errorf("interrupt cover listener accept: %w", err)
			}
		}
	}
	for _, b := range c.packets {
		if b.endpoint.Role == "cover" && b.acceptCancel != nil {
			b.acceptCancel()
		}
	}
	stop := context.AfterFunc(ctx, func() { c.mu.Lock(); c.cond.Broadcast(); c.mu.Unlock() })
	defer stop()
	for {
		pending := false
		for _, b := range c.bindings {
			pending = pending || (b.endpoint.Role == "cover" && b.inAccept)
		}
		for _, b := range c.packets {
			pending = pending || (b.endpoint.Role == "cover" && b.inAccept)
		}
		if !pending {
			return nil
		}
		if err := ctx.Err(); err != nil {
			return err
		}
		c.cond.Wait()
	}
}

func (c *tcpHandoff) resume() error {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.closed || !c.ready || len(c.bindings)+len(c.packets) != len(c.expected) {
		return fmt.Errorf("handoff listener group is not ready")
	}
	for _, b := range c.bindings {
		if b.closed {
			return fmt.Errorf("selected TCP handoff listener is closed")
		}
	}
	for _, b := range c.packets {
		if b.closed {
			return fmt.Errorf("UDP handoff listener is closed")
		}
	}
	for _, b := range c.bindings {
		if err := b.listener.SetDeadline(time.Time{}); err != nil {
			return err
		}
	}
	c.paused = false
	c.coverPaused = false
	c.drainRequested = false
	c.cond.Broadcast()
	return nil
}

func (c *tcpHandoff) close() error {
	c.opMu.Lock()
	c.mu.Lock()
	c.closed = true
	c.paused = true
	var failure error
	for _, b := range c.bindings {
		if !b.closed {
			failure = errors.Join(failure, b.listener.SetDeadline(time.Now()))
		}
	}
	for _, b := range c.packets {
		if b.acceptCancel != nil {
			b.acceptCancel()
			b.acceptCancel = nil
		}
	}
	c.cond.Broadcast()
	c.mu.Unlock()
	if c.packetRelease != nil {
		c.packetRelease()
	}
	failure = errors.Join(failure, c.control.Close())
	c.opMu.Unlock()
	c.worker.Wait()
	return failure
}

func (c *tcpHandoff) serve() {
	defer c.worker.Done()
	for {
		conn, err := c.control.AcceptUnix()
		if err != nil {
			return
		}
		// One bounded request at a time keeps descriptors, goroutines and pending
		// state mutations bounded even when a trusted local manager misbehaves.
		c.serveConnection(conn)
		conn.Close()
	}
}

func (c *tcpHandoff) serveConnection(conn *net.UnixConn) {
	if err := conn.SetDeadline(time.Now().Add(handoffTimeout)); err != nil {
		return
	}
	if err := checkHandoffPeer(conn); err != nil {
		return
	}
	buffer := make([]byte, handoffMaxMessage+1)
	n, _, flags, _, err := conn.ReadMsgUnix(buffer, nil)
	if err != nil {
		return
	}
	var request handoffRequest
	if flags&(unix.MSG_TRUNC|unix.MSG_CTRUNC) != 0 {
		err = fmt.Errorf("TCP handoff request is truncated or contains ancillary data")
	} else {
		err = decodeHandoffJSON(buffer[:n], &request)
	}
	var fds []int
	var response handoffResponse
	if err == nil {
		response, fds, err = c.execute(request)
	}
	defer func() {
		for _, fd := range fds {
			unix.Close(fd)
		}
	}()
	if err != nil {
		response = handoffResponse{Error: err.Error()}
	}
	body, err := json.Marshal(response)
	if err != nil {
		return
	}
	var rights []byte
	if len(fds) > 0 {
		rights = unix.UnixRights(fds...)
	}
	_, _, _ = conn.WriteMsgUnix(body, rights, nil)
}

func (c *tcpHandoff) execute(request handoffRequest) (handoffResponse, []int, error) {
	c.opMu.Lock()
	defer c.opMu.Unlock()
	if request.GenerationID != c.manifest.GenerationID {
		return handoffResponse{}, nil, fmt.Errorf("TCP handoff generation identity mismatch")
	}
	switch request.Operation {
	case "status":
	case "seal", "pause", "resume", "export", "drain", "drop_udp", "drop_tcp":
		c.mu.Lock()
		valid := !c.closed && request.Epoch > 0 && validHandoffIntent(request.Intent) && request.Epoch >= c.epoch
		if request.Epoch == c.epoch {
			valid = valid && request.Intent == c.intent && request.Operation == c.operation
		}
		if valid {
			c.epoch, c.intent, c.operation = request.Epoch, request.Intent, request.Operation
		}
		c.mu.Unlock()
		if !valid {
			return handoffResponse{}, nil, fmt.Errorf("stale or conflicting TCP handoff fencing intent")
		}
	default:
		return handoffResponse{}, nil, fmt.Errorf("unsupported TCP handoff operation")
	}
	var err error
	var fds []int
	switch request.Operation {
	case "seal":
		err = c.seal()
	case "pause":
		ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
		err = c.pause(ctx)
		cancel()
	case "resume":
		err = c.resume()
	case "drain":
		ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
		err = c.drain(ctx)
		cancel()
	case "drop_udp":
		err = c.dropUDP()
	case "drop_tcp":
		err = c.dropTCP()
	case "export":
		fds, err = c.exportListeners()
	}
	if err != nil {
		return handoffResponse{}, nil, err
	}
	s := c.status()
	return handoffResponse{Status: &s}, fds, nil
}

func (c *tcpHandoff) exportListeners() ([]int, error) {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.closed || !c.ready || !c.paused || len(c.bindings)+len(c.packets) != len(c.expected) {
		return nil, fmt.Errorf("TCP handoff export requires the complete selected group to be ready and paused")
	}
	addresses := make([]string, 0, len(c.bindings))
	for address := range c.bindings {
		addresses = append(addresses, address)
	}
	sort.Strings(addresses)
	fds := make([]int, 0, len(addresses))
	for _, address := range addresses {
		b := c.bindings[address]
		// The first pause barrier deliberately leaves REALITY cover admission running
		// until the candidate has restored its dynamic state. Duplicating the listening
		// socket is safe while the old generation is blocked in Accept; only listeners
		// that the barrier promised to pause must have left Accept before export.
		if b.closed || (b.endpoint.Role != "cover" && b.inAccept) {
			for _, fd := range fds {
				unix.Close(fd)
			}
			return nil, fmt.Errorf(
				"TCP handoff %s listener %s has not completed its pause barrier",
				b.endpoint.Role,
				address,
			)
		}
		identity, err := inspectHandoffListener(b.listener)
		if err == nil && identity != b.identity {
			err = fmt.Errorf("TCP handoff listener socket identity changed")
		}
		fd := -1
		if err == nil {
			fd, err = duplicateHandoffListener(b.listener)
		}
		if err != nil {
			for _, ownedFD := range fds {
				unix.Close(ownedFD)
			}
			return nil, err
		}
		fds = append(fds, fd)
	}
	return fds, nil
}

func matchesHandoffEndpoint(identity handoffSocketIdentity, endpoint handoffEndpoint) bool {
	return identity.Address == endpoint.BindAddress && identity.SocketInode == endpoint.SocketInode && identity.SocketCookie == endpoint.SocketCookie && identity.Protocol == endpoint.Protocol
}

// Go's "tcp" listener may implement an explicit 0.0.0.0 bind with one [::]
// dual-stack socket. That socket is equivalent only while IPV6_V6ONLY is off;
// accepting the textual wildcard mismatch without checking the kernel option
// would make the manifest claim IPv4 coverage that the listener does not have.
func handoffListenerMatchesRequestedAddress(listener *net.TCPListener, actualAddress, requestedAddress string) bool {
	if actualAddress == requestedAddress {
		return true
	}
	actual, actualErr := net.ResolveTCPAddr("tcp", actualAddress)
	requested, requestedErr := net.ResolveTCPAddr("tcp", requestedAddress)
	if actualErr != nil || requestedErr != nil || actual.Port != requested.Port || !actual.IP.IsUnspecified() || actual.IP.To4() != nil || !requested.IP.IsUnspecified() || requested.IP.To4() == nil {
		return false
	}
	raw, err := listener.SyscallConn()
	if err != nil {
		return false
	}
	v6Only, optionErr := 1, error(nil)
	var identity handoffSocketIdentity
	if err := raw.Control(func(fd uintptr) {
		v6Only, optionErr = unix.GetsockoptInt(int(fd), unix.IPPROTO_IPV6, unix.IPV6_V6ONLY)
		if errors.Is(optionErr, unix.EOPNOTSUPP) {
			identity, _ = inspectHandoffFD(int(fd))
		}
	}); err != nil {
		return false
	}
	if errors.Is(optionErr, unix.EOPNOTSUPP) && identity.Protocol == unix.IPPROTO_MPTCP && identity.Address == actualAddress {
		// Linux 5.15 exposes the listening subflow's v6only flag through SOCK_DIAG
		// even when the MPTCP getsockopt entry point cannot return it. Match the
		// held socket's inode and address; never infer dual-stack from a wildcard.
		v6Only, optionErr = handoffMPTCPV6Only(identity)
	}
	if optionErr != nil {
		return false
	}
	return v6Only == 0
}

// Go's "udp" packet listener has the same wildcard behavior as "tcp": an
// explicit IPv4 wildcard request may be backed by one dual-stack IPv6 socket.
// A zero requested port also permits the kernel-selected generation-private
// port. Both relaxations still require an equivalent IP bind, and the IPv6
// wildcard is accepted only after verifying that it also receives IPv4.
func handoffPacketMatchesRequestedAddress(conn *net.UDPConn, actualAddress, requestedAddress string) bool {
	actual, actualErr := net.ResolveUDPAddr("udp", actualAddress)
	requested, requestedErr := net.ResolveUDPAddr("udp", requestedAddress)
	if actualErr != nil || requestedErr != nil || (requested.Port != 0 && actual.Port != requested.Port) {
		return false
	}
	if actual.IP.Equal(requested.IP) {
		return true
	}
	if !actual.IP.IsUnspecified() || actual.IP.To4() != nil || !requested.IP.IsUnspecified() || requested.IP.To4() == nil {
		return false
	}
	raw, err := conn.SyscallConn()
	if err != nil {
		return false
	}
	v6Only, optionErr := 1, error(nil)
	if err := raw.Control(func(fd uintptr) {
		v6Only, optionErr = unix.GetsockoptInt(int(fd), unix.IPPROTO_IPV6, unix.IPV6_V6ONLY)
	}); err != nil || optionErr != nil {
		return false
	}
	return v6Only == 0
}

func inspectHandoffFD(fd int) (handoffSocketIdentity, error) {
	var identity handoffSocketIdentity
	var st unix.Stat_t
	if err := unix.Fstat(fd, &st); err != nil {
		return identity, err
	}
	if st.Mode&unix.S_IFMT != unix.S_IFSOCK {
		return identity, fmt.Errorf("TCP handoff FD is not a socket")
	}
	for option, expected := range map[int]int{unix.SO_TYPE: unix.SOCK_STREAM, unix.SO_ACCEPTCONN: 1} {
		actual, err := unix.GetsockoptInt(fd, unix.SOL_SOCKET, option)
		if err != nil || actual != expected {
			return identity, fmt.Errorf("TCP handoff FD is not a listening stream socket")
		}
	}
	protocol, err := unix.GetsockoptInt(fd, unix.SOL_SOCKET, unix.SO_PROTOCOL)
	if err != nil || (protocol != unix.IPPROTO_TCP && protocol != unix.IPPROTO_MPTCP) {
		return identity, fmt.Errorf("TCP handoff FD is not TCP or MPTCP")
	}
	flags, err := unix.FcntlInt(uintptr(fd), unix.F_GETFL, 0)
	if err != nil || flags&unix.O_NONBLOCK == 0 {
		return identity, fmt.Errorf("TCP handoff FD must already be nonblocking")
	}
	address, err := unix.Getsockname(fd)
	if err != nil {
		return identity, err
	}
	var tcpAddress net.TCPAddr
	switch address := address.(type) {
	case *unix.SockaddrInet4:
		tcpAddress = net.TCPAddr{IP: net.IP(address.Addr[:]), Port: address.Port}
	case *unix.SockaddrInet6:
		if address.ZoneId != 0 {
			return identity, fmt.Errorf("scoped IPv6 TCP handoff is unsupported")
		}
		tcpAddress = net.TCPAddr{IP: net.IP(address.Addr[:]), Port: address.Port}
	default:
		return identity, fmt.Errorf("TCP handoff requires an IP socket")
	}
	cookie, err := unix.GetsockoptUint64(fd, unix.SOL_SOCKET, unix.SO_COOKIE)
	if err != nil || cookie == 0 || st.Ino == 0 {
		return identity, fmt.Errorf("TCP handoff socket identity is unavailable")
	}
	return handoffSocketIdentity{Address: tcpAddress.String(), SocketInode: st.Ino, SocketCookie: cookie, Protocol: protocol}, nil
}

func inspectHandoffListener(listener *net.TCPListener) (handoffSocketIdentity, error) {
	var identity handoffSocketIdentity
	raw, err := listener.SyscallConn()
	if err != nil {
		return identity, err
	}
	var failure error
	if err := raw.Control(func(fd uintptr) { identity, failure = inspectHandoffFD(int(fd)) }); err != nil {
		return identity, err
	}
	return identity, failure
}

func inspectHandoffPacketConn(conn *net.UDPConn) (handoffSocketIdentity, error) {
	var identity handoffSocketIdentity
	raw, err := conn.SyscallConn()
	if err != nil {
		return identity, err
	}
	var failure error
	if err := raw.Control(func(rawFD uintptr) {
		fd := int(rawFD)
		var st unix.Stat_t
		if err := unix.Fstat(fd, &st); err != nil {
			failure = err
			return
		}
		typeValue, err := unix.GetsockoptInt(fd, unix.SOL_SOCKET, unix.SO_TYPE)
		if err != nil || typeValue != unix.SOCK_DGRAM {
			failure = fmt.Errorf("UDP handoff FD is not a datagram socket")
			return
		}
		protocol, err := unix.GetsockoptInt(fd, unix.SOL_SOCKET, unix.SO_PROTOCOL)
		if err != nil || protocol != unix.IPPROTO_UDP {
			failure = fmt.Errorf("UDP handoff FD is not UDP")
			return
		}
		address, err := unix.Getsockname(fd)
		if err != nil {
			failure = err
			return
		}
		var udpAddress net.UDPAddr
		switch address := address.(type) {
		case *unix.SockaddrInet4:
			udpAddress = net.UDPAddr{IP: net.IP(address.Addr[:]), Port: address.Port}
		case *unix.SockaddrInet6:
			if address.ZoneId != 0 {
				failure = fmt.Errorf("scoped IPv6 UDP handoff is unsupported")
				return
			}
			udpAddress = net.UDPAddr{IP: net.IP(address.Addr[:]), Port: address.Port}
		default:
			failure = fmt.Errorf("UDP handoff requires an IP socket")
			return
		}
		cookie, err := unix.GetsockoptUint64(fd, unix.SOL_SOCKET, unix.SO_COOKIE)
		if err != nil || cookie == 0 || st.Ino == 0 {
			failure = fmt.Errorf("UDP handoff socket identity is unavailable")
			return
		}
		identity = handoffSocketIdentity{Address: udpAddress.String(), SocketInode: st.Ino, SocketCookie: cookie, Protocol: protocol}
	}); err != nil {
		return identity, err
	}
	return identity, failure
}

// TCPListener.File().Fd() can change the shared open-file description to
// blocking mode. Duplicating under RawConn.Control preserves O_NONBLOCK for the
// old listener, including if the candidate never starts or fails validation.
func duplicateHandoffListener(listener *net.TCPListener) (int, error) {
	raw, err := listener.SyscallConn()
	if err != nil {
		return -1, err
	}
	fd := -1
	var failure error
	if err := raw.Control(func(original uintptr) { fd, failure = unix.FcntlInt(original, unix.F_DUPFD_CLOEXEC, 3) }); err != nil {
		return -1, err
	}
	return fd, failure
}

func checkHandoffPeer(conn *net.UnixConn) error {
	raw, err := conn.SyscallConn()
	if err != nil {
		return err
	}
	var peer *unix.Ucred
	var failure error
	if err := raw.Control(func(fd uintptr) { peer, failure = unix.GetsockoptUcred(int(fd), unix.SOL_SOCKET, unix.SO_PEERCRED) }); err != nil {
		return err
	}
	if failure != nil || peer == nil || peer.Uid != uint32(os.Geteuid()) {
		return fmt.Errorf("TCP handoff control peer credentials rejected")
	}
	return nil
}
