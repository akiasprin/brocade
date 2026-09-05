package anytls

import (
	"context"
	"crypto/sha256"
	"encoding/binary"
	"sync"
	"sync/atomic"
	"time"

	"github.com/xtls/xray-core/common/buf"
	"github.com/xtls/xray-core/common/errors"
	"github.com/xtls/xray-core/common/protocol"
	"github.com/xtls/xray-core/common/retry"
	sessionctx "github.com/xtls/xray-core/common/session"
	core "github.com/xtls/xray-core/core"
	"github.com/xtls/xray-core/features/policy"
	"github.com/xtls/xray-core/transport"
	"github.com/xtls/xray-core/transport/internet"
	"github.com/xtls/xray-core/transport/internet/stat"
)

const (
	defaultIdleSessionCheckInterval = 30 * time.Second
	defaultIdleSessionTimeout       = 30 * time.Second
	defaultMinIdleSession           = 0
)

type Client struct {
	server        *protocol.ServerSpec
	policyManager policy.Manager

	idleSessionCheckInterval time.Duration
	idleSessionTimeout       time.Duration
	minIdleSession           int

	defaultPaddingScheme *paddingScheme
	authPadding          uint16
	authHash             [32]byte
	paddingMu            sync.RWMutex

	poolMu       sync.Mutex
	idleSessions []uint64
	sessionsMu   sync.Mutex
	sessions     map[uint64]*session
	sessionSeq   atomic.Uint64

	closeOnce   sync.Once
	closed      atomic.Bool
	cleanupDone chan struct{}
}

func NewClient(ctx context.Context, config *ClientConfig) (*Client, error) {
	if config == nil || config.Server == nil {
		return nil, errors.New("anytls: no server specified")
	}

	server, err := protocol.NewServerSpecFromPB(config.Server)
	if err != nil {
		return nil, errors.New("failed to get server spec").Base(err)
	}
	if server.User == nil {
		return nil, errors.New("anytls: no user specified")
	}
	account, ok := server.User.Account.(*MemoryAccount)
	if !ok {
		return nil, errors.New("anytls: invalid account type")
	}

	v := core.MustFromContext(ctx)
	client := &Client{
		server:                   server,
		policyManager:            v.GetFeature(policy.ManagerType()).(policy.Manager),
		idleSessionCheckInterval: defaultIdleSessionCheckInterval,
		idleSessionTimeout:       defaultIdleSessionTimeout,
		minIdleSession:           defaultMinIdleSession,
		defaultPaddingScheme:     getDefaultPaddingScheme(),
		authHash:                 sha256.Sum256([]byte(account.Password)),
		sessions:                 make(map[uint64]*session),
		cleanupDone:              make(chan struct{}),
	}
	client.authPadding = getPadding0Size(client.defaultPaddingScheme)
	if value := config.GetIdleSessionCheckInterval(); value > 0 {
		client.idleSessionCheckInterval = time.Duration(value) * time.Second
	}
	if value := config.GetIdleSessionTimeout(); value > 0 {
		client.idleSessionTimeout = time.Duration(value) * time.Second
	}
	client.minIdleSession = int(config.GetMinIdleSession())
	go client.cleanupIdleSessions()
	return client, nil
}

func (c *Client) isClosed() bool {
	return c == nil || c.closed.Load()
}

func (c *Client) paddingSnapshot() (*paddingScheme, uint16) {
	c.paddingMu.RLock()
	defer c.paddingMu.RUnlock()
	return c.defaultPaddingScheme, c.authPadding
}

func (c *Client) updatePaddingScheme(scheme *paddingScheme) {
	c.paddingMu.Lock()
	c.defaultPaddingScheme = scheme
	c.authPadding = getPadding0Size(scheme)
	c.paddingMu.Unlock()
}

func (c *Client) Close() error {
	if c == nil {
		return nil
	}

	c.closeOnce.Do(func() {
		c.closed.Store(true)
		if c.cleanupDone != nil {
			close(c.cleanupDone)
		}

		c.poolMu.Lock()
		c.sessionsMu.Lock()
		sessions := make([]*session, 0, len(c.sessions))
		for _, sess := range c.sessions {
			if sess != nil {
				sess.inIdlePool.Store(false)
				sessions = append(sessions, sess)
			}
		}
		c.idleSessions = nil
		c.sessions = make(map[uint64]*session)
		c.sessionsMu.Unlock()
		c.poolMu.Unlock()

		closeErr := errors.New("anytls: client closed")
		for _, sess := range sessions {
			sess.close(closeErr)
		}
	})
	return nil
}

func (c *Client) Process(ctx context.Context, link *transport.Link, dialer internet.Dialer) error {
	if c.isClosed() {
		return errors.New("anytls: client closed")
	}
	outbounds := sessionctx.OutboundsFromContext(ctx)
	if len(outbounds) == 0 {
		return errors.New("target not specified")
	}
	ob := outbounds[len(outbounds)-1]
	if !ob.Target.IsValid() {
		return errors.New("target not specified")
	}
	ob.Name = "anytls"
	ob.CanSpliceCopy = 3 // Multiplexed AnyTLS frames must be encoded before delivery.
	destination := ob.Target

	var sess *session
	reusedSession := false
	c.poolMu.Lock()
	if c.isClosed() {
		c.poolMu.Unlock()
		return errors.New("anytls: client closed")
	}
	c.sessionsMu.Lock()
	for len(c.idleSessions) > 0 {
		last := len(c.idleSessions) - 1
		seq := c.idleSessions[last]
		c.idleSessions = c.idleSessions[:last]
		sess = c.sessions[seq]
		if sess == nil {
			continue
		}
		sess.inIdlePool.Store(false)
		if sess.isClosed() {
			sess = nil
			continue
		}
		reusedSession = true
		break
	}
	c.sessionsMu.Unlock()
	c.poolMu.Unlock()

	if sess == nil {
		var err error
		sess, err = c.createSession(ctx, dialer)
		if err != nil {
			return err
		}
	}

	stream, err := sess.openStream(ctx, destination, link)
	if err == errSessionClosed && reusedSession {
		sess.close(err)
		sess, err = c.createSession(ctx, dialer)
		if err != nil {
			return err
		}
		stream, err = sess.openStream(ctx, destination, link)
	}
	if err != nil {
		sess.close(err)
		return errors.New("anytls: failed to open stream").Base(err)
	}
	stream.setDieHook(func() {
		if sess.isClosed() || sess.activeStreams.Load() != 0 {
			return
		}
		c.markSessionIdle(sess)
	})
	go stream.pumpUplink(sess)

	select {
	case <-stream.done:
		return stream.result()
	case <-ctx.Done():
		return ctx.Err()
	}
}

func (c *Client) createSession(ctx context.Context, dialer internet.Dialer) (*session, error) {
	if c.isClosed() {
		return nil, errors.New("anytls: client closed")
	}

	seq := c.sessionSeq.Add(1)
	var conn stat.Connection
	err := retry.ExponentialBackoff(5, 100).On(func() error {
		rawConn, err := dialer.Dial(ctx, c.server.Destination)
		if err != nil {
			return err
		}
		conn = rawConn
		return nil
	})
	if err != nil {
		return nil, errors.New("anytls: failed to establish connection").AtWarning().Base(err)
	}

	paddingScheme, authPadding := c.paddingSnapshot()
	auth := make([]byte, 34+int(authPadding))
	copy(auth[:32], c.authHash[:])
	binary.BigEndian.PutUint16(auth[32:34], authPadding)
	if err := writeFull(conn, auth); err != nil {
		_ = conn.Close()
		return nil, errors.New("anytls: write auth failed").Base(err)
	}
	if c.isClosed() {
		_ = conn.Close()
		return nil, errors.New("anytls: client closed")
	}

	sess := &session{
		client:        c,
		isClient:      true,
		conn:          conn,
		br:            &buf.BufferedReader{Reader: buf.NewReader(conn)},
		bw:            buf.NewBufferedWriter(buf.NewWriter(conn)),
		paddingScheme: paddingScheme,
		streams:       make(map[uint32]*stream),
		seq:           seq,
	}
	sess.fw = newFrameWriter(sess.bw)
	sess.nextSID.Store(1)
	sess.pktCounter.Store(1)
	sess.setPeerVersion(1)
	sess.setDieHook(func() {
		c.sessionsMu.Lock()
		delete(c.sessions, seq)
		c.sessionsMu.Unlock()
	})
	c.poolMu.Lock()
	if c.isClosed() {
		c.poolMu.Unlock()
		_ = conn.Close()
		return nil, errors.New("anytls: client closed")
	}
	c.sessionsMu.Lock()
	c.sessions[seq] = sess
	c.sessionsMu.Unlock()
	c.poolMu.Unlock()
	errors.LogDebug(ctx, "anytls: new session created, seq=", seq)

	go func() {
		if err := sess.readLoop(ctx); err != nil && !sess.isClosed() {
			sess.close(err)
		}
	}()
	return sess, nil
}

func (c *Client) markSessionIdle(sess *session) {
	if sess == nil {
		return
	}
	if sess.nextSID.Load() == 0 {
		sess.close(errStreamIDExhausted)
		return
	}

	c.poolMu.Lock()
	defer c.poolMu.Unlock()
	if c.isClosed() || sess.isClosed() {
		return
	}
	if !sess.inIdlePool.CompareAndSwap(false, true) {
		return
	}
	sess.idleSinceNano.Store(time.Now().UnixNano())
	c.idleSessions = append(c.idleSessions, sess.seq)
}

func (c *Client) cleanupIdleSessions() {
	ticker := time.NewTicker(c.idleSessionCheckInterval)
	defer ticker.Stop()

	for {
		select {
		case <-ticker.C:
			c.cleanupIdleSessionsAt(time.Now())
		case <-c.cleanupDone:
			return
		}
	}
}

func (c *Client) cleanupIdleSessionsAt(now time.Time) {
	if c.isClosed() {
		return
	}
	var toClose []*session

	c.poolMu.Lock()
	if len(c.idleSessions) == 0 {
		c.poolMu.Unlock()
		return
	}

	c.sessionsMu.Lock()
	validCount := 0
	for _, seq := range c.idleSessions {
		sess := c.sessions[seq]
		if sess == nil || sess.isClosed() || !sess.inIdlePool.Load() {
			continue
		}
		c.idleSessions[validCount] = seq
		validCount++
	}
	c.idleSessions = c.idleSessions[:validCount]

	keepFrom := validCount - c.minIdleSession
	if keepFrom < 0 {
		keepFrom = 0
	}

	keptCount := 0
	for idx, seq := range c.idleSessions {
		sess := c.sessions[seq]
		if sess == nil {
			continue
		}
		if idx >= keepFrom {
			c.idleSessions[keptCount] = seq
			keptCount++
			continue
		}
		idleSinceNano := sess.idleSinceNano.Load()
		if idleSinceNano == 0 || now.Sub(time.Unix(0, idleSinceNano)) <= c.idleSessionTimeout {
			c.idleSessions[keptCount] = seq
			keptCount++
			continue
		}
		sess.inIdlePool.Store(false)
		toClose = append(toClose, sess)
	}
	c.sessionsMu.Unlock()
	c.idleSessions = c.idleSessions[:keptCount]
	c.poolMu.Unlock()

	for _, sess := range toClose {
		sess.close(nil)
	}
}
