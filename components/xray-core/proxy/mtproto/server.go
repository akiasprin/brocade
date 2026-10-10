package mtproto

import (
	"bytes"
	"context"
	"crypto/cipher"
	"crypto/sha256"
	"encoding/binary"
	"io"
	"sort"
	"sync"
	"time"

	"github.com/xtls/xray-core/common/crypto"
	"github.com/xtls/xray-core/common/errors"
	xnet "github.com/xtls/xray-core/common/net"
	"github.com/xtls/xray-core/common/protocol"
	"github.com/xtls/xray-core/common/session"
	"github.com/xtls/xray-core/common/signal"
	"github.com/xtls/xray-core/core"
	"github.com/xtls/xray-core/features/policy"
	"github.com/xtls/xray-core/features/routing"
	featurestats "github.com/xtls/xray-core/features/stats"
	"github.com/xtls/xray-core/transport/internet/stat"
)

var validConnectionTypes = map[[4]byte]struct{}{
	{0xef, 0xef, 0xef, 0xef}: {}, // abridged
	{0xee, 0xee, 0xee, 0xee}: {}, // intermediate
	{0xdd, 0xdd, 0xdd, 0xdd}: {}, // padded intermediate (dd secret links)
}

const (
	replayTTL                     = 5 * time.Minute
	maxReplayItems                = 32768
	onlineObservationGrace        = 2 * time.Minute
	maxRetainedOnlineObservations = 4096
)

type replayEntry struct {
	digest    [32]byte
	expiresAt time.Time
}

type Server struct {
	policyManager policy.Manager
	statsManager  featurestats.Manager
	onlineTracker *onlineObservationTracker
	directory     *proxyDirectory

	userMu       sync.RWMutex
	usersByEmail map[string]*protocol.MemoryUser
	users        []*protocol.MemoryUser
	active       map[*protocol.MemoryUser]map[stat.Connection]struct{}

	replayMu    sync.Mutex
	replaySeen  map[[32]byte]time.Time
	replayQueue []replayEntry
	replayHead  int
}

func NewServer(ctx context.Context, config *ServerConfig) (*Server, error) {
	if config == nil {
		return nil, errors.New("mtproto: server config is required")
	}
	v := core.MustFromContext(ctx)
	statsManager, _ := v.GetFeature(featurestats.ManagerType()).(featurestats.Manager)
	server := &Server{
		policyManager: v.GetFeature(policy.ManagerType()).(policy.Manager),
		statsManager:  statsManager,
		onlineTracker: newOnlineObservationTracker(
			onlineObservationGrace,
			maxRetainedOnlineObservations,
		),
		directory:    newOfficialProxyDirectory(),
		usersByEmail: make(map[string]*protocol.MemoryUser),
		active:       make(map[*protocol.MemoryUser]map[stat.Connection]struct{}),
		replaySeen:   make(map[[32]byte]time.Time),
	}
	for _, user := range config.Users {
		memory, err := user.ToMemoryUser()
		if err != nil {
			return nil, errors.New("mtproto: invalid user").Base(err)
		}
		if err := server.addUser(memory); err != nil {
			return nil, err
		}
	}
	return server, nil
}

func (s *Server) Network() []xnet.Network {
	return []xnet.Network{xnet.Network_TCP}
}

// Process terminates Telegram's obfuscated transport and opens the official middle-proxy RPC
// socket itself. That socket's real local and remote endpoints are inputs to Telegram's AES key
// derivation, so it cannot be replaced with Xray's abstract dispatcher pipe. Consequently this
// inbound deliberately does not participate in SNI/content sniffing or destination-based Xray
// routing: MTProxy carries only Telegram packets and selects a DC from its own initialization
// header.
func (s *Server) Process(ctx context.Context, network xnet.Network, conn stat.Connection, _ routing.Dispatcher) error {
	basePolicy := s.policyManager.ForLevel(0)
	if err := conn.SetDeadline(time.Now().Add(basePolicy.Timeouts.Handshake)); err != nil {
		return errors.New("mtproto: set handshake deadline").Base(err)
	}

	raw := make([]byte, headerSize)
	if _, err := io.ReadFull(conn, raw); err != nil {
		return errors.New("mtproto: read authentication header").Base(err)
	}

	user, auth, dcID := s.authenticate(raw)
	if user == nil {
		return errors.New("mtproto: invalid authentication")
	}
	if s.replayed(raw, time.Now()) {
		return errors.New("mtproto: replayed authentication header")
	}

	if err := conn.SetDeadline(time.Time{}); err != nil {
		return errors.New("mtproto: clear handshake deadline").Base(err)
	}
	if !s.registerConnection(user, conn) {
		return errors.New("mtproto: user was revoked")
	}
	defer s.unregisterConnection(user, conn)

	inbound := session.InboundFromContext(ctx)
	inbound.Name = protocolName
	inbound.User = user
	inbound.CanSpliceCopy.Store(session.SpliceCopyDisabled)
	userPolicy := s.policyManager.ForLevel(user.Level)
	conn, stopAccounting := accountConnection(
		s.statsManager,
		s.onlineTracker,
		userPolicy,
		user.Email,
		inbound.Source.Address.String(),
		conn,
	)
	defer stopAccounting()
	inbound.Conn = conn

	ctx, cancel := context.WithCancel(ctx)
	defer cancel()
	timer := signal.CancelAfterInactivity(ctx, cancel, userPolicy.Timeouts.ConnectionIdle)
	defer timer.SetTimeout(0)

	upstreamCtx, stopUpstreamHandshake := context.WithTimeout(ctx, 15*time.Second)
	target, proxySecret, err := s.directory.target(upstreamCtx, dcID)
	if err != nil {
		stopUpstreamHandshake()
		return errors.New("mtproto: load official Telegram proxy directory").Base(err)
	}
	middle, err := dialMiddleRPC(upstreamCtx, target, proxySecret)
	stopUpstreamHandshake()
	if err != nil {
		return errors.New("mtproto: connect to official Telegram middle proxy").Base(err)
	}
	defer middle.Close()
	transport := newClientTransport(conn, auth)
	stopCancellation := context.AfterFunc(ctx, func() {
		_ = conn.Close()
		_ = middle.Close()
	})
	defer stopCancellation()

	request := func() error {
		defer timer.SetTimeout(userPolicy.Timeouts.DownlinkOnly)
		for {
			payload, flags, err := transport.readPacket()
			if err != nil {
				return err
			}
			validationFlags, err := validateMTProtoRequest(payload)
			if err != nil {
				return err
			}
			packet, err := proxyRequest(middle.connectionID, flags|validationFlags, conn.RemoteAddr(), conn.LocalAddr(), payload)
			if err != nil {
				return err
			}
			if err := middle.writePacket(packet); err != nil {
				return err
			}
			timer.Update()
		}
	}
	response := func() error {
		defer timer.SetTimeout(userPolicy.Timeouts.UplinkOnly)
		for {
			_, packet, err := middle.readPacket()
			if err != nil {
				return err
			}
			if len(packet) < 4 {
				return errors.New("mtproto: short official middle proxy response")
			}
			switch binary.LittleEndian.Uint32(packet[:4]) {
			case rpcProxyAnswer:
				if len(packet) < 16 || int64(binary.LittleEndian.Uint64(packet[8:16])) != middle.connectionID {
					return errors.New("mtproto: invalid official middle proxy answer")
				}
				if err := transport.writePacket(packet[16:]); err != nil {
					return err
				}
			case rpcSimpleAck:
				if len(packet) != 16 || int64(binary.LittleEndian.Uint64(packet[4:12])) != middle.connectionID {
					return errors.New("mtproto: invalid official middle proxy acknowledgement")
				}
				if err := transport.writeQuickAck(binary.LittleEndian.Uint32(packet[12:16])); err != nil {
					return err
				}
			case rpcCloseExternal:
				if len(packet) != 12 || int64(binary.LittleEndian.Uint64(packet[4:12])) != middle.connectionID {
					return errors.New("mtproto: invalid official middle proxy close")
				}
				return io.EOF
			case rpcPing:
				if len(packet) != 12 {
					return errors.New("mtproto: invalid official middle proxy ping")
				}
				binary.LittleEndian.PutUint32(packet[:4], rpcPong)
				if err := middle.writePacket(packet); err != nil {
					return err
				}
			default:
				continue
			}
			timer.Update()
		}
	}

	errorsChannel := make(chan error, 2)
	go func() { errorsChannel <- request() }()
	go func() { errorsChannel <- response() }()
	err = <-errorsChannel
	cancel()
	_ = conn.Close()
	_ = middle.Close()
	return errors.New("mtproto: connection ended").Base(err)
}

// accountConnection mirrors the user accounting normally installed by dispatcher.getLink. The
// MTProxy data path cannot use that dispatcher because Telegram's middle-proxy key derivation
// depends on the real upstream socket endpoints, so it has to attach the same counters and online
// map directly after authentication. The 64-byte authentication header is deliberately excluded,
// just as the ordinary dispatched protocols exclude their own handshake from user traffic.
func accountConnection(
	manager featurestats.Manager,
	onlineTracker *onlineObservationTracker,
	userPolicy policy.Session,
	email string,
	sourceIP string,
	conn stat.Connection,
) (stat.Connection, func()) {
	if manager == nil || email == "" {
		return conn, func() {}
	}

	var uplinkCounter, downlinkCounter featurestats.Counter
	if userPolicy.Stats.UserUplink {
		uplinkCounter, _ = featurestats.GetOrRegisterCounter(
			manager,
			"user>>>"+email+">>>traffic>>>uplink",
		)
	}
	if userPolicy.Stats.UserDownlink {
		downlinkCounter, _ = featurestats.GetOrRegisterCounter(
			manager,
			"user>>>"+email+">>>traffic>>>downlink",
		)
	}
	if uplinkCounter != nil || downlinkCounter != nil {
		conn = &stat.CounterConnection{
			Connection:   conn,
			ReadCounter:  uplinkCounter,
			WriteCounter: downlinkCounter,
		}
	}

	if !userPolicy.Stats.UserOnline || sourceIP == "" {
		return conn, func() {}
	}
	if onlineTracker != nil {
		return conn, onlineTracker.observe(manager, email, sourceIP)
	}
	online, _ := featurestats.GetOrRegisterOnlineMap(manager, "user>>>"+email+">>>online")
	if online == nil {
		return conn, func() {}
	}
	if protocols, ok := online.(featurestats.ProtocolOnlineMap); ok {
		protocols.AddIPWithProtocol(sourceIP, protocolName)
		return conn, func() { protocols.RemoveIPWithProtocol(sourceIP, protocolName) }
	}
	online.AddIP(sourceIP)
	return conn, func() { online.RemoveIP(sourceIP) }
}

// Telegram encrypts the complete 64-byte initialization payload before sending its first 56
// bytes in clear and the final eight encrypted. Both peers keep using that CTR stream for the
// payload, so a freshly constructed stream must consume the initialization block first.
func requestPayloadStream(key [32]byte, nonce [16]byte) cipher.Stream {
	stream := crypto.NewAesCTRStream(key[:], nonce[:])
	var initialization [headerSize]byte
	stream.XORKeyStream(initialization[:], initialization[:])
	return stream
}

func (s *Server) authenticate(raw []byte) (*protocol.MemoryUser, *authentication, int16) {
	s.userMu.RLock()
	users := append([]*protocol.MemoryUser(nil), s.users...)
	s.userMu.RUnlock()

	var matched *protocol.MemoryUser
	var matchedAuth *authentication
	var matchedDC int16
	for _, user := range users {
		account, ok := user.Account.(*MemoryAccount)
		if !ok {
			continue
		}
		auth, err := readAuthentication(bytes.NewReader(raw))
		if err != nil {
			continue
		}
		auth.applySecret(account.Secret)
		crypto.NewAesCTRStream(auth.decodingKey[:], auth.decodingNonce[:]).XORKeyStream(auth.header[:], auth.header[:])
		_, connectionTypeOK := validConnectionTypes[auth.connectionType()]
		dcID, dcOK := auth.dataCenterID()
		if connectionTypeOK && dcOK && matched == nil {
			matched, matchedAuth, matchedDC = user, auth, dcID
		}
	}
	return matched, matchedAuth, matchedDC
}

func (s *Server) replayed(header []byte, now time.Time) bool {
	digest := sha256.Sum256(header)
	s.replayMu.Lock()
	defer s.replayMu.Unlock()

	for s.replayHead < len(s.replayQueue) && !s.replayQueue[s.replayHead].expiresAt.After(now) {
		entry := s.replayQueue[s.replayHead]
		if s.replaySeen[entry.digest] == entry.expiresAt {
			delete(s.replaySeen, entry.digest)
		}
		s.replayHead++
	}
	if expiresAt, ok := s.replaySeen[digest]; ok && expiresAt.After(now) {
		return true
	}
	for len(s.replaySeen) >= maxReplayItems && s.replayHead < len(s.replayQueue) {
		entry := s.replayQueue[s.replayHead]
		delete(s.replaySeen, entry.digest)
		s.replayHead++
	}
	expiresAt := now.Add(replayTTL)
	s.replaySeen[digest] = expiresAt
	s.replayQueue = append(s.replayQueue, replayEntry{digest: digest, expiresAt: expiresAt})
	if s.replayHead > 4096 && s.replayHead*2 > len(s.replayQueue) {
		s.replayQueue = append([]replayEntry(nil), s.replayQueue[s.replayHead:]...)
		s.replayHead = 0
	}
	return false
}

func (s *Server) AddUser(ctx context.Context, user *protocol.MemoryUser) error {
	return s.addUser(user)
}

func (s *Server) addUser(user *protocol.MemoryUser) error {
	if user == nil || user.Email == "" {
		return errors.New("mtproto: user email is required")
	}
	if _, ok := user.Account.(*MemoryAccount); !ok {
		return errors.New("mtproto: invalid account")
	}
	s.userMu.Lock()
	defer s.userMu.Unlock()
	if _, exists := s.usersByEmail[user.Email]; exists {
		return errors.New("mtproto: user email already exists")
	}
	for _, existing := range s.users {
		if existing.Account.Equals(user.Account) {
			return errors.New("mtproto: user secret already exists")
		}
	}
	s.usersByEmail[user.Email] = user
	s.users = append(s.users, user)
	sort.Slice(s.users, func(i, j int) bool { return s.users[i].Email < s.users[j].Email })
	return nil
}

func (s *Server) RemoveUser(ctx context.Context, email string) error {
	if email == "" {
		return errors.New("mtproto: user email is required")
	}
	s.userMu.Lock()
	user := s.usersByEmail[email]
	delete(s.usersByEmail, email)
	for index, candidate := range s.users {
		if candidate == user {
			s.users = append(s.users[:index], s.users[index+1:]...)
			break
		}
	}
	connections := make([]stat.Connection, 0, len(s.active[user]))
	for conn := range s.active[user] {
		connections = append(connections, conn)
	}
	delete(s.active, user)
	s.userMu.Unlock()
	for _, conn := range connections {
		_ = conn.Close()
	}
	return nil
}

func (s *Server) GetUser(ctx context.Context, email string) *protocol.MemoryUser {
	s.userMu.RLock()
	defer s.userMu.RUnlock()
	return s.usersByEmail[email]
}

func (s *Server) GetUsers(ctx context.Context) []*protocol.MemoryUser {
	s.userMu.RLock()
	defer s.userMu.RUnlock()
	return append([]*protocol.MemoryUser(nil), s.users...)
}

func (s *Server) GetUsersCount(context.Context) int64 {
	s.userMu.RLock()
	defer s.userMu.RUnlock()
	return int64(len(s.users))
}

func (s *Server) registerConnection(user *protocol.MemoryUser, conn stat.Connection) bool {
	s.userMu.Lock()
	defer s.userMu.Unlock()
	if s.usersByEmail[user.Email] != user {
		return false
	}
	if s.active[user] == nil {
		s.active[user] = make(map[stat.Connection]struct{})
	}
	s.active[user][conn] = struct{}{}
	return true
}

func (s *Server) unregisterConnection(user *protocol.MemoryUser, conn stat.Connection) {
	s.userMu.Lock()
	defer s.userMu.Unlock()
	delete(s.active[user], conn)
	if len(s.active[user]) == 0 {
		delete(s.active, user)
	}
}
