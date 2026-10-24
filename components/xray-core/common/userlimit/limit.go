// Package userlimit admits authenticated payload through the Agent's shared user budgets.
// Business identity comes from authentication or a managed forwarding listener, never source IP.
package userlimit

import (
	"bufio"
	"context"
	"encoding/json"
	"errors"
	"github.com/xtls/xray-core/common/platform/filesystem"
	"github.com/xtls/xray-core/common/session"
	"io"
	"net"
	"os"
	"strconv"
	"strings"
	"sync"
	"time"
)

const Env = "XRAY_BROCADE_USER_LIMIT_FILE"
const StatsName = "brocade>>>user_limit>>>supported"

type contextKey struct{}
type control struct {
	Endpoint   string `json:"endpoint"`
	Token      string `json:"token"`
	Generation uint64 `json:"generation"`
}
type connection struct {
	net.Conn
	reader  *bufio.Reader
	control control
}
type freeState struct {
	generation uint64
	until      time.Time
}
type Client struct {
	path      string
	mu        sync.Mutex
	current   control
	refreshed time.Time
	idle      []*connection
	slots     chan struct{}
	free      map[string]freeState
}

func NewContext(ctx context.Context) context.Context {
	if path := os.Getenv(Env); path != "" {
		return context.WithValue(ctx, contextKey{}, New(path))
	}
	return ctx
}
func New(path string) *Client {
	return &Client{path: path, slots: make(chan struct{}, 32), free: make(map[string]freeState)}
}
func FromContext(ctx context.Context) *Client { c, _ := ctx.Value(contextKey{}).(*Client); return c }
func Configured() bool                        { return os.Getenv(Env) != "" }
func Source(ctx context.Context) string {
	inbound := session.InboundFromContext(ctx)
	if outbounds := session.OutboundsFromContext(ctx); len(outbounds) > 0 {
		target := outbounds[len(outbounds)-1].OriginalTarget
		if target.Address != nil && target.Address.Family().IsDomain() && target.Address.Domain() == "v1.mux.cool" {
			return ""
		}
	}
	if inbound == nil || inbound.Tag == "api" {
		return ""
	}
	if strings.HasPrefix(inbound.Tag, "fwd:") {
		return inbound.Tag
	}
	if inbound.User != nil && strings.Contains(inbound.User.Email, "@") && strings.Contains(inbound.User.Email, "#") {
		return inbound.User.Email
	}
	return ""
}
func (c *Client) load() (control, error) {
	c.mu.Lock()
	defer c.mu.Unlock()
	if !c.refreshed.IsZero() && time.Since(c.refreshed) < 250*time.Millisecond {
		return c.current, nil
	}
	f, err := filesystem.OpenControlFile(c.path)
	if err != nil {
		return control{}, errors.New("user budget control unavailable")
	}
	defer f.Close()
	info, err := f.Stat()
	if err != nil || !info.Mode().IsRegular() {
		return control{}, errors.New("invalid user budget control")
	}
	data, err := io.ReadAll(io.LimitReader(f, 4097))
	if err != nil || len(data) > 4096 {
		return control{}, errors.New("invalid user budget control")
	}
	var cfg control
	if json.Unmarshal(data, &cfg) != nil {
		return cfg, errors.New("invalid user budget control")
	}
	host, port, err := net.SplitHostPort(cfg.Endpoint)
	ip := net.ParseIP(host)
	p, e := strconv.Atoi(port)
	if err != nil || e != nil || ip == nil || !ip.IsLoopback() || p < 1 || p > 65535 || len(cfg.Token) != 32 || strings.Trim(cfg.Token, "0123456789abcdef") != "" {
		return cfg, errors.New("invalid user budget endpoint")
	}
	if cfg != c.current {
		c.free = make(map[string]freeState)
		for _, conn := range c.idle {
			_ = conn.Close()
		}
		c.idle = nil
	}
	c.current = cfg
	c.refreshed = time.Now()
	return cfg, nil
}
func (c *Client) Ready() bool {
	if c == nil {
		return false
	}
	_, err := c.load()
	return err == nil
}
func Wait(ctx context.Context, n int) error {
	c := FromContext(ctx)
	source := Source(ctx)
	if c == nil || source == "" || n <= 0 {
		return nil
	}
	for n > 0 {
		part := min(n, 16384)
		if err := c.wait(ctx, source, part); err != nil {
			return err
		}
		n -= part
	}
	return nil
}
func (c *Client) wait(ctx context.Context, source string, n int) error {
	cfg, err := c.load()
	if err != nil {
		return err
	}
	c.mu.Lock()
	state, ok := c.free[source]
	c.mu.Unlock()
	if ok && state.generation == cfg.Generation && (state.until.IsZero() || time.Now().Before(state.until)) {
		return nil
	}

	err = c.request(ctx, source, n)
	if err == nil || ctx.Err() != nil {
		return err
	}
	c.mu.Lock()
	c.refreshed = time.Time{}
	c.mu.Unlock()
	// An idle loopback connection may have expired or belong to the preceding Agent process.
	// A lost admission only wastes bandwidth credits; retrying cannot release unbudgeted payload.
	return c.request(ctx, source, n)
}
func (c *Client) request(ctx context.Context, source string, n int) error {
	if len(source) > 512 {
		return errors.New("invalid user budget source")
	}
	select {
	case c.slots <- struct{}{}:
	case <-ctx.Done():
		return ctx.Err()
	}
	defer func() { <-c.slots }()
	cfg, err := c.load()
	if err != nil {
		return err
	}
	c.mu.Lock()
	var conn *connection
	if len(c.idle) > 0 {
		conn = c.idle[len(c.idle)-1]
		c.idle = c.idle[:len(c.idle)-1]
	}
	c.mu.Unlock()
	if conn == nil {
		raw, e := (&net.Dialer{Timeout: 2 * time.Second}).DialContext(ctx, "tcp", cfg.Endpoint)
		if e != nil {
			return errors.New("user budget unavailable")
		}
		conn = &connection{Conn: raw, reader: bufio.NewReaderSize(raw, 128), control: cfg}
	}
	healthy := false
	defer func() {
		if healthy {
			c.mu.Lock()
			if len(c.idle) < 32 && conn.control == c.current {
				c.idle = append(c.idle, conn)
				conn = nil
			}
			c.mu.Unlock()
		}
		if conn != nil {
			_ = conn.Close()
		}
	}()
	done := make(chan struct{})
	stop := context.AfterFunc(ctx, func() { _ = conn.Close(); close(done) })
	defer func() {
		if !stop() {
			<-done
		}
	}()
	deadline := func() {
		at := time.Now().Add(10 * time.Second)
		if d, ok := ctx.Deadline(); ok && d.Before(at) {
			at = d
		}
		_ = conn.SetDeadline(at)
	}
	deadline()
	payload, _ := json.Marshal(struct {
		Token  string `json:"token"`
		Source string `json:"source"`
		Bytes  int    `json:"bytes"`
	}{cfg.Token, source, n})
	if _, err = conn.Write(append(payload, '\n')); err != nil {
		return errors.New("user budget write failed")
	}
	for {
		line, e := conn.reader.ReadSlice('\n')
		if e != nil {
			return errors.New("user budget unavailable")
		}
		switch string(line) {
		case "WAIT\n":
			deadline()
		case "OPEN\n", "PASS\n":
			state := freeState{generation: cfg.Generation}
			if string(line) == "OPEN\n" {
				state.until = time.Now().Add(time.Second)
			}
			c.mu.Lock()
			if len(c.free) < 4096 {
				c.free[source] = state
			}
			c.mu.Unlock()
			healthy = true
			return nil
		case "OK\n":
			healthy = true
			return nil
		case "STOP\n":
			return errors.New("user service is suspended")
		default:
			return errors.New("user budget rejected")
		}
	}
}
