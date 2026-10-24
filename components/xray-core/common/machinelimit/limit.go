// Package machinelimit shares a machine budget across concurrent Xray generations.
// The private control file is portable; only the loopback Agent owns token scheduling.
package machinelimit

import (
	"bufio"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"github.com/xtls/xray-core/common/userlimit"
	"io"
	"net"
	"os"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"
	"time"
)

const StatsName = "brocade>>>machine_limit>>>rate_bps"
const Env = "XRAY_BROCADE_MACHINE_LIMIT_FILE"
const RequiredEnv = "XRAY_BROCADE_MACHINE_LIMIT_REQUIRED"

var monotonicOrigin = time.Now()

type contextKey struct{}
type config struct {
	Enabled  bool   `json:"enabled"`
	Rate     uint64 `json:"rate_bps"`
	Progress bool   `json:"progress"`
	Endpoint string `json:"endpoint"`
	Token    string `json:"token"`
}
type state struct {
	config
	err error
}
type Controller struct {
	path            string
	required        bool
	next            atomic.Int64
	state           atomic.Pointer[state]
	refreshMu       sync.Mutex
	gate            chan struct{}
	conn            net.Conn
	reader          *bufio.Reader
	endpoint, token string
}

func NewContext(ctx context.Context) context.Context {
	ctx = userlimit.NewContext(ctx)
	path := os.Getenv(Env)
	if path == "" {
		if userlimit.FromContext(ctx) != nil {
			c := New("")
			c.required = false
			return WithController(ctx, c)
		}
		return ctx
	}
	c := New(path)
	c.required = os.Getenv(RequiredEnv) != "0"
	return WithController(ctx, c)
}
func New(path string) *Controller {
	return &Controller{path: path, required: true, gate: make(chan struct{}, 1)}
}
func WithController(ctx context.Context, c *Controller) context.Context {
	return context.WithValue(ctx, contextKey{}, c)
}
func FromContext(ctx context.Context) *Controller {
	c, _ := ctx.Value(contextKey{}).(*Controller)
	return c
}

func (c *Controller) load(force bool) (*state, error) {
	if c == nil {
		return &state{}, nil
	}
	now := time.Since(monotonicOrigin).Nanoseconds()
	if !force && now < c.next.Load() {
		s := c.state.Load()
		if s != nil {
			return s, s.err
		}
	}
	c.refreshMu.Lock()
	defer c.refreshMu.Unlock()
	if !force && now < c.next.Load() {
		s := c.state.Load()
		if s != nil {
			return s, s.err
		}
	}
	s := new(state)
	f, err := openControl(c.path)
	if err == nil {
		var data []byte
		info, statErr := f.Stat()
		if statErr != nil || !info.Mode().IsRegular() {
			err = errors.New("control must be a regular file")
		} else {
			data, err = io.ReadAll(io.LimitReader(f, 4097))
		}
		_ = f.Close()
		if len(data) > 4096 {
			err = errors.New("oversized control file")
		}
		if err == nil {
			err = json.Unmarshal(data, &s.config)
		}
	}
	if err == nil {
		host, portText, e := net.SplitHostPort(s.Endpoint)
		address := net.ParseIP(host)
		port, portErr := strconv.Atoi(portText)
		if e != nil || portErr != nil || address == nil || !address.IsLoopback() || port <= 0 || port > 65535 || len(s.Token) != 32 || strings.Trim(s.Token, "0123456789abcdef") != "" || (s.Rate != 0 && s.Rate < 1000) || s.Rate > 1_000_000_000_000 || (!s.Enabled && s.Rate != 0) {
			err = errors.New("invalid control file")
		}
	}
	if err != nil {
		previous := c.state.Load()
		inactive := previous != nil && previous.err == nil && (!previous.Enabled || previous.Rate == 0)
		if inactive || (previous == nil && !c.required) {
			s = &state{}
		} else {
			s.err = errors.New("machine limit control unavailable")
		}
	}
	c.state.Store(s)
	c.next.Store(now + int64(250*time.Millisecond))
	return s, s.err
}

// ChunkSize bounds queued payload and allows hot changes to reach existing buffered/splice loops.
func (c *Controller) ChunkSize() (int, error) {
	s, err := c.load(false)
	if err != nil {
		return 0, err
	}
	if !s.Enabled || s.Rate == 0 {
		if userlimit.Configured() {
			return 16384, nil
		}
		return 1 << 20, nil
	}
	return int(max(uint64(1), min(uint64(16384), s.Rate/80))), nil
}
func (c *Controller) Wait(ctx context.Context, n int) error {
	if err := userlimit.Wait(ctx, n); err != nil {
		return err
	}
	s, err := c.load(false)
	if err != nil {
		return err
	}
	if c == nil || !s.Enabled || s.Rate == 0 {
		return nil
	}
	for n > 0 {
		s, err = c.load(false)
		if err != nil {
			return err
		}
		if !s.Enabled || s.Rate == 0 {
			return nil
		}
		// Keep one request near 100 ms of budget, including for an atomic UDP datagram.
		// Release the connection gate between chunks so one slow datagram cannot starve TCP.
		part := min(n, int(max(uint64(1), min(uint64(16384), s.Rate/80))))
		if _, err = c.request(ctx, s, part); err != nil {
			return err
		}
		n -= part
	}
	return nil
}
func (c *Controller) Snapshot(ctx context.Context) (uint64, error) {
	if c == nil {
		return 0, errors.New("machine limit unsupported")
	}
	s, err := c.load(true)
	if err != nil {
		return 0, err
	}
	if !s.Enabled {
		return 0, nil
	}
	// Probes use a separate connection so a busy transfer queue cannot starve readback.
	probe := New(c.path)
	defer func() {
		if probe.conn != nil {
			_ = probe.conn.Close()
		}
	}()
	return probe.request(ctx, s, 0)
}
func (c *Controller) request(ctx context.Context, s *state, n int) (uint64, error) {
	select {
	case c.gate <- struct{}{}:
	case <-ctx.Done():
		return 0, ctx.Err()
	}
	defer func() { <-c.gate }()
	lastProgress := time.Now()
	retry := func() error {
		if time.Since(lastProgress) >= 10*time.Second {
			return errors.New("machine budget unavailable")
		}
		timer := time.NewTimer(100 * time.Millisecond)
		defer timer.Stop()
		select {
		case <-timer.C:
		case <-ctx.Done():
			return ctx.Err()
		}
		latest, err := c.load(true)
		if err != nil {
			return err
		}
		s = latest
		return nil
	}
	for {
		if !s.Enabled || s.Rate == 0 {
			return 0, nil
		}
		if c.conn != nil && (c.endpoint != s.Endpoint || c.token != s.Token) {
			_ = c.conn.Close()
			c.conn = nil
		}
		if c.conn == nil {
			dialer := net.Dialer{Timeout: 2 * time.Second}
			conn, err := dialer.DialContext(ctx, "tcp", s.Endpoint)
			if err != nil {
				if err = retry(); err != nil {
					return 0, errors.New("machine budget unavailable")
				}
				continue
			}
			if tcp, ok := conn.(*net.TCPConn); ok {
				_ = tcp.SetNoDelay(true)
			}
			c.conn = conn
			c.reader = bufio.NewReaderSize(conn, 128)
			c.endpoint = s.Endpoint
			c.token = s.Token
		}
		conn := c.conn
		setDeadline := func() {
			deadline := time.Now().Add(10 * time.Second)
			if parent, ok := ctx.Deadline(); ok && parent.Before(deadline) {
				deadline = parent
			}
			_ = conn.SetDeadline(deadline)
		}
		setDeadline()
		callbackDone := make(chan struct{})
		stop := context.AfterFunc(ctx, func() { _ = conn.Close(); close(callbackDone) })
		progress := ""
		if s.Progress {
			progress = " progress-v1"
		}
		_, err := fmt.Fprintf(conn, "%s %d%s\n", s.Token, n, progress)
		var raw []byte
		for err == nil {
			raw, err = c.reader.ReadSlice('\n')
			if err != nil || string(raw) != "WAIT\n" {
				break
			}
			lastProgress = time.Now()
			setDeadline()
		}
		if !stop() {
			<-callbackDone
		}
		line := string(raw)
		if err == nil && len(line) <= 64 && strings.HasPrefix(line, "OK ") {
			rate, e := strconv.ParseUint(strings.TrimSpace(strings.TrimPrefix(line, "OK ")), 10, 64)
			if e == nil && rate <= 1_000_000_000_000 {
				return rate, nil
			}
		}
		_ = conn.Close()
		c.conn = nil
		if err = retry(); err != nil {
			return 0, errors.New("machine budget request failed")
		}
	}
}
