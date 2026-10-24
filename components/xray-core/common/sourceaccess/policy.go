// Package sourceaccess enforces the Agent's durable source-IP admission snapshot.
// Every Xray generation reads the same private file; policy and session admission
// share a lock so an authentication racing with a ban cannot escape revocation.
package sourceaccess

import (
	"context"
	"encoding/json"
	"errors"
	"io"
	"net"
	"net/netip"
	"os"
	"strings"
	"sync"
	"time"
)

const Env = "XRAY_BROCADE_SOURCE_ACCESS_FILE"
const RevisionStat = "brocade>>>source_access>>>revision"
const HealthyStat = "brocade>>>source_access>>>healthy"
const MaxEntries = 8192
const maxFile = 2 * 1024 * 1024

type UserBlock struct {
	Tenant string   `json:"tenant_id"`
	User   string   `json:"user_id"`
	IPs    []string `json:"ips"`
}
type Policy struct {
	Revision int64       `json:"revision"`
	NodeIPs  []string    `json:"node_ips"`
	Users    []UserBlock `json:"users"`
}
type counter interface{ Set(int64) int64 }
type key struct{}
type identity struct{ tenant, user string }
type entry struct {
	user  identity
	ip    netip.Addr
	close func()
	peer  func() net.Addr
}
type Manager struct {
	workers           sync.WaitGroup
	updateMu          sync.Mutex
	receiptRevision   int64
	receiptHealthy    bool
	receiptWritten    bool
	path              string
	mu                sync.Mutex
	policy            Policy
	node              map[netip.Addr]bool
	users             map[identity]map[netip.Addr]bool
	active            map[*entry]struct{}
	revision, healthy counter
	done              chan struct{}
	stop              sync.Once
}

func Configured() bool                         { return os.Getenv(Env) != "" }
func FromContext(ctx context.Context) *Manager { m, _ := ctx.Value(key{}).(*Manager); return m }
func Context(ctx context.Context, m *Manager) context.Context {
	return context.WithValue(ctx, key{}, m)
}
func New(path string) (*Manager, error) {
	m := &Manager{path: path, active: make(map[*entry]struct{}), done: make(chan struct{})}
	if err := m.reload(); err != nil {
		return nil, err
	}
	return m, nil
}
func (m *Manager) Type() interface{} { return (*Manager)(nil) }
func (m *Manager) Start() error {
	m.workers.Add(1)
	go func() {
		defer m.workers.Done()
		ticker := time.NewTicker(250 * time.Millisecond)
		defer ticker.Stop()
		for {
			select {
			case <-ticker.C:
				_ = m.reload()
			case <-m.done:
				return
			}
		}
	}()
	return nil
}
func (m *Manager) Close() error {
	m.stop.Do(func() { close(m.done); m.workers.Wait(); _ = os.Remove(m.receiptPath()) })
	return nil
}
func (m *Manager) SetCounters(revision, healthy counter) {
	m.mu.Lock()
	defer m.mu.Unlock()
	m.revision, m.healthy = revision, healthy
	if revision != nil {
		revision.Set(m.policy.Revision)
	}
	if healthy != nil {
		healthy.Set(1)
	}
}
func (m *Manager) reload() error {
	m.updateMu.Lock()
	defer m.updateMu.Unlock()
	m.mu.Lock()
	stopPeers := m.closeDeniedLocked()
	m.mu.Unlock()
	for _, stop := range stopPeers {
		stop()
	}
	// O_NONBLOCK prevents a damaged checkpoint replaced with a FIFO from hanging startup.
	f, err := openPolicy(m.path)
	if err != nil {
		m.unhealthy()
		return errors.New("source access checkpoint unavailable")
	}
	defer f.Close()
	info, err := f.Stat()
	if err != nil || !info.Mode().IsRegular() {
		m.unhealthy()
		return errors.New("invalid source access checkpoint")
	}
	data, err := io.ReadAll(io.LimitReader(f, maxFile+1))
	if err != nil || len(data) > maxFile {
		m.unhealthy()
		return errors.New("source access checkpoint too large")
	}
	var policy Policy
	if json.Unmarshal(data, &policy) != nil {
		m.unhealthy()
		return errors.New("invalid source access policy")
	}
	return m.applyLocked(policy)
}
func (m *Manager) unhealthy() {
	m.receiptHealthy = false
	_ = m.writeReceipt(m.policy.Revision, false)
	m.mu.Lock()
	defer m.mu.Unlock()
	if m.healthy != nil {
		m.healthy.Set(0)
	}
}
func parseIP(value string) (netip.Addr, error) {
	ip, err := netip.ParseAddr(value)
	if err != nil || ip.Zone() != "" {
		return netip.Addr{}, errors.New("invalid source IP")
	}
	return ip.Unmap(), nil
}
func (m *Manager) apply(policy Policy) error {
	m.updateMu.Lock()
	defer m.updateMu.Unlock()
	return m.applyLocked(policy)
}
func (m *Manager) applyLocked(policy Policy) error {
	node := make(map[netip.Addr]bool)
	users := make(map[identity]map[netip.Addr]bool)
	count := len(policy.NodeIPs)
	valid := policy.Revision >= 0
	for _, value := range policy.NodeIPs {
		ip, err := parseIP(value)
		if err != nil {
			valid = false
		}
		node[ip] = true
	}
	for _, user := range policy.Users {
		id := identity{user.Tenant, user.User}
		if id.tenant == "" || id.user == "" || strings.ContainsAny(id.tenant+id.user, "@#") {
			valid = false
		}
		if users[id] != nil {
			valid = false
		}
		ips := make(map[netip.Addr]bool)
		for _, value := range user.IPs {
			ip, err := parseIP(value)
			if err != nil {
				valid = false
			}
			ips[ip] = true
		}
		users[id] = ips
		count += len(user.IPs)
	}
	if !valid || count > MaxEntries || len(policy.Users) > MaxEntries {
		m.unhealthy()
		return errors.New("invalid source access policy")
	}
	m.mu.Lock()
	if policy.Revision < m.policy.Revision {
		m.mu.Unlock()
		m.unhealthy()
		return errors.New("source access revision regressed")
	}
	m.policy, m.node, m.users = policy, node, users
	close := m.closeDeniedLocked()
	// Publish the receipt only after every matching session has been closed.
	m.mu.Unlock()
	for _, stop := range close {
		stop()
	}
	if !m.receiptWritten || m.receiptRevision != policy.Revision || !m.receiptHealthy {
		if err := m.writeReceipt(policy.Revision, true); err != nil {
			m.unhealthy()
			return errors.New("source access receipt unavailable")
		}
		m.receiptWritten, m.receiptRevision, m.receiptHealthy = true, policy.Revision, true
	}
	m.mu.Lock()
	if m.revision != nil {
		m.revision.Set(policy.Revision)
	}
	if m.healthy != nil {
		m.healthy.Set(1)
	}
	m.mu.Unlock()
	return nil
}

// QUIC can migrate to a different peer without another authentication. Recheck
// its current transport address even when the policy revision did not change.
func (m *Manager) closeDeniedLocked() []func() {
	var close []func()
	for entry := range m.active {
		valid := true
		if entry.peer != nil {
			ip, err := peerIP(entry.peer())
			valid = err == nil
			entry.ip = ip
		}
		if !valid || m.denied(entry.user, entry.ip) {
			delete(m.active, entry)
			close = append(close, entry.close)
		}
	}
	return close
}
func peerIP(addr net.Addr) (netip.Addr, error) {
	if addr == nil {
		return netip.Addr{}, errors.New("source access peer unavailable")
	}
	host, _, err := net.SplitHostPort(addr.String())
	if err != nil {
		return netip.Addr{}, errors.New("source access peer unavailable")
	}
	ip, err := netip.ParseAddr(host)
	if err != nil {
		return netip.Addr{}, errors.New("source access peer unavailable")
	}
	// Interface scope is socket metadata, not a separate address in the blacklist.
	return ip.WithZone("").Unmap(), nil
}
func (m *Manager) denied(user identity, ip netip.Addr) bool { return m.node[ip] || m.users[user][ip] }
func userIdentity(label string) identity {
	user, tail, ok := strings.Cut(label, "@")
	if !ok {
		return identity{}
	}
	tenant, _, ok := strings.Cut(tail, "#")
	if !ok {
		return identity{}
	}
	return identity{tenant, user}
}

// Register owns the actual authenticated carrier (a full QUIC session for Hy2).
// Node-only registrations use an empty label. API listeners never call this.
func Register(ctx context.Context, label string, addr net.Addr, close func()) (func(), error) {
	return register(ctx, label, addr, nil, close)
}
func RegisterPeer(ctx context.Context, label string, peer func() net.Addr, close func()) (func(), error) {
	if FromContext(ctx) == nil {
		return func() {}, nil
	}
	if peer == nil {
		return nil, errors.New("source access peer unavailable")
	}
	return register(ctx, label, peer(), peer, close)
}
func register(ctx context.Context, label string, addr net.Addr, peer func() net.Addr, close func()) (func(), error) {
	m := FromContext(ctx)
	if m == nil {
		return func() {}, nil
	}
	if close == nil {
		return nil, errors.New("source access close callback unavailable")
	}
	ip, err := peerIP(addr)
	if err != nil {
		return nil, err
	}
	e := &entry{user: userIdentity(label), ip: ip, close: close, peer: peer}
	m.mu.Lock()
	defer m.mu.Unlock()
	if m.denied(e.user, ip) {
		return nil, errors.New("source IP access denied")
	}
	m.active[e] = struct{}{}
	return func() { m.mu.Lock(); delete(m.active, e); m.mu.Unlock() }, nil
}
