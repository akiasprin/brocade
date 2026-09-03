package anytls

import (
	"context"
	"crypto/sha256"
	"encoding/binary"
	"io"
	"sync"
	"time"

	"github.com/xtls/xray-core/common/buf"
	"github.com/xtls/xray-core/common/errors"
	xnet "github.com/xtls/xray-core/common/net"
	"github.com/xtls/xray-core/common/protocol"
	sessionctx "github.com/xtls/xray-core/common/session"
	"github.com/xtls/xray-core/core"
	"github.com/xtls/xray-core/features/policy"
	"github.com/xtls/xray-core/features/routing"
	"github.com/xtls/xray-core/transport/internet/stat"
)

type Server struct {
	policyManager policy.Manager
	users         map[[32]byte]*protocol.MemoryUser
	usersByEmail  map[string]*protocol.MemoryUser
	userMu        sync.RWMutex
	paddingScheme string
	padding       *paddingScheme
	masquerade    *masquerade
}

func NewServer(ctx context.Context, config *ServerConfig) (*Server, error) {
	if config == nil {
		return nil, errors.New("anytls: server config is required")
	}
	v := core.MustFromContext(ctx)
	rawPaddingScheme := config.PaddingScheme
	parsedPaddingScheme := getDefaultPaddingScheme()
	if rawPaddingScheme == "" {
		rawPaddingScheme = string(parsedPaddingScheme.rawScheme)
	} else {
		var err error
		parsedPaddingScheme, err = parsePaddingScheme(rawPaddingScheme)
		if err != nil {
			return nil, errors.New("anytls: invalid padding scheme").Base(err)
		}
	}
	s := &Server{
		policyManager: v.GetFeature(policy.ManagerType()).(policy.Manager),
		users:         make(map[[32]byte]*protocol.MemoryUser),
		usersByEmail:  make(map[string]*protocol.MemoryUser),
		paddingScheme: rawPaddingScheme,
		padding:       parsedPaddingScheme,
	}
	var err error
	s.masquerade, err = newMasquerade(config.Masquerade)
	if err != nil {
		return nil, errors.New("anytls: invalid masquerade").Base(err)
	}
	for _, u := range config.Users {
		mu, err := u.ToMemoryUser()
		if err != nil {
			return nil, errors.New("anytls: bad user").Base(err)
		}
		acc, ok := mu.Account.(*MemoryAccount)
		if !ok {
			return nil, errors.New("anytls: user account type")
		}
		sum := sha256.Sum256([]byte(acc.Password))
		s.users[sum] = mu
		s.usersByEmail[mu.Email] = mu
	}
	return s, nil
}

func (s *Server) Network() []xnet.Network {
	return []xnet.Network{xnet.Network_TCP, xnet.Network_UNIX}
}

func (s *Server) Process(ctx context.Context, network xnet.Network, conn stat.Connection, dispatcher routing.Dispatcher) error {
	sessPol := s.policyManager.ForLevel(0)
	handshakeDeadline := time.Now().Add(sessPol.Timeouts.Handshake)
	_ = conn.SetReadDeadline(handshakeDeadline)

	sess := &session{
		isClient:      false,
		server:        s,
		conn:          conn,
		br:            &buf.BufferedReader{Reader: buf.NewReader(conn)},
		bw:            buf.NewBufferedWriter(buf.NewWriter(conn)),
		streams:       make(map[uint32]*stream),
		dispatcher:    dispatcher,
		paddingScheme: s.padding,
	}
	sess.fw = newFrameWriter(sess.bw)
	sess.peerVersion = 1
	sess.pktCounter.Store(1)
	failAuthentication := func(err error) error {
		masquerade := s.masquerade
		if masquerade == nil {
			masquerade = defaultMasquerade404()
		}
		// The read deadline may already have expired (for example, when a
		// browser sends an incomplete request). Give the masquerade response
		// its own bounded write window.
		_ = conn.SetWriteDeadline(time.Now().Add(sessPol.Timeouts.Handshake))
		_ = masquerade.write(conn)
		_ = conn.SetWriteDeadline(time.Time{})
		return err
	}

	// auth header: 32B sha256(password) + 2B padlen
	var h [34]byte
	if _, err := io.ReadFull(sess.br, h[:]); err != nil {
		return failAuthentication(errors.New("anytls: read auth").Base(err))
	}

	var sum [32]byte
	copy(sum[:], h[:32])
	s.userMu.RLock()
	user := s.users[sum]
	s.userMu.RUnlock()
	if user == nil {
		return failAuthentication(errors.New("anytls: invalid user"))
	}

	padlen := binary.BigEndian.Uint16(h[32:34])
	if padlen > 0 {
		if err := discardBytes(sess.br, int(padlen)); err != nil {
			return failAuthentication(errors.New("anytls: read padding0").Base(err))
		}
	}
	_ = conn.SetReadDeadline(time.Time{})
	_ = conn.SetWriteDeadline(time.Time{})

	inb := sessionctx.InboundFromContext(ctx)
	inb.Name = protocolName
	inb.User = user
	inb.CanSpliceCopy = 3

	return sess.readLoop(ctx)
}
