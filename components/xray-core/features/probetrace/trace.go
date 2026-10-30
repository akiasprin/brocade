// Package probetrace carries bounded evidence inside authenticated Brocade probe streams.
// It never changes the public proxy protocols or observes ordinary user connections.
package probetrace

import (
	"context"
	"crypto/rand"
	"encoding/hex"
	"encoding/json"
	"errors"
	"io"
	"os"
	"strings"
	"sync"

	"github.com/xtls/xray-core/common/net"
	"github.com/xtls/xray-core/common/session"
	"github.com/xtls/xray-core/features/dns"
)

const Version = 2
const MaxFrame = 8192
const MaxFile = 2 * 1024 * 1024
const MaxRecords = 512
const NodeFileEnv = "XRAY_BROCADE_PROBE_IDENTITY_FILE"
const FileEnv = "XRAY_BROCADE_PROBE_FILE"
const ProfileEnv = "XRAY_BROCADE_PROBE_PROFILE"
const LocalTag = "probe:request:"

type Result struct {
	Version        int               `json:"version"`
	ID             string            `json:"id"`
	Domain         string            `json:"domain"`
	Exit           string            `json:"exit"`
	Outbound       string            `json:"outbound"`
	Profile        string            `json:"profile"`
	NormalStrategy string            `json:"normal_strategy"`
	Family         string            `json:"family,omitempty"`
	Connected      bool              `json:"connected"`
	Complete       bool              `json:"complete"`
	Error          string            `json:"error,omitempty"`
	Lookups        []dns.LookupTrace `json:"lookups"`
}
type Record struct {
	Group   string `json:"group"`
	Started bool   `json:"started,omitempty"`
	Result
}
type contextKey struct{}
type State struct {
	ID     string
	Domain string
	once   sync.Once
	mu     sync.Mutex
	result *Result
	emit   func(Result) error
	local  bool
}

func FromContext(ctx context.Context) *State {
	value, _ := ctx.Value(contextKey{}).(*State)
	return value
}
func WithoutState(ctx context.Context) context.Context {
	return context.WithValue(ctx, contextKey{}, (*State)(nil))
}
func WithState(ctx context.Context, state *State) context.Context {
	// The private evidence envelope must pass through its reader/writer wrappers.
	if inbound := session.InboundFromContext(ctx); inbound != nil {
		inbound.CanSpliceCopy.Store(session.SpliceCopyDisabled)
	}
	return context.WithValue(ctx, contextKey{}, state)
}
func Identifier(value string, limit int) bool {
	if len(value) == 0 || len(value) > limit {
		return false
	}
	for _, c := range value {
		if !(c >= 'a' && c <= 'z' || c >= 'A' && c <= 'Z' || c >= '0' && c <= '9' || strings.ContainsRune(":._-", c)) {
			return false
		}
	}
	return true
}
func Domain(value string) bool { return Identifier(value, 253) && !strings.ContainsAny(value, ":_") }
func validID(id string) bool {
	bytes, err := hex.DecodeString(id)
	return err == nil && len(bytes) == 16 && id == strings.ToLower(id)
}
func (r *Result) Valid() bool {
	if r.Version != Version || !validID(r.ID) || !Domain(r.Domain) || len(r.Lookups) > 8 || len(r.Error) > 32 {
		return false
	}
	if r.Exit != "" && !Identifier(r.Exit, 128) || r.Outbound != "" && !Identifier(r.Outbound, 256) {
		return false
	}
	if r.Family != "" && r.Family != "v4" && r.Family != "v6" {
		return false
	}
	if len(r.NormalStrategy) > 32 || r.Profile != "" && len(r.Profile) != 64 {
		return false
	}
	for _, lookup := range r.Lookups {
		if len(lookup.Steps) > 8 {
			return false
		}
		for _, step := range lookup.Steps {
			if !Identifier(step.Tag, 64) || step.Outcome != "answer" && step.Outcome != "empty" && step.Outcome != "error" {
				return false
			}
		}
	}
	return !r.Connected || r.Family != ""
}
func (s *State) Set(result Result) {
	s.set(result, true)
}
func (s *State) set(result Result, emit bool) {
	accepted := false
	s.once.Do(func() {
		result.Version = Version
		result.ID = s.ID
		result.Domain = s.Domain
		if !result.Valid() {
			result = Result{Version: Version, ID: s.ID, Domain: s.Domain, Error: "invalid_evidence"}
		}
		encoded, _ := json.Marshal(result)
		if len(encoded) > MaxFrame {
			result.Lookups = nil
			result.Complete = false
			result.Error = "evidence_limit"
		}
		if result.Lookups == nil {
			result.Lookups = make([]dns.LookupTrace, 0)
		}
		for i := range result.Lookups {
			if result.Lookups[i].Steps == nil {
				result.Lookups[i].Steps = make([]dns.LookupTraceStep, 0)
			}
		}
		s.mu.Lock()
		s.result = &result
		s.mu.Unlock()
		accepted = true
	})
	// Network writes must not hold once: cancellation needs to close that same
	// stream even while a peer is applying backpressure to its evidence frame.
	if accepted && emit && s.emit != nil {
		_ = s.emit(result)
	}
}
func (s *State) Unknown() { s.Set(Result{Error: "missing_evidence"}) }
func (s *State) Abort()   { s.set(Result{Error: "missing_evidence"}, s.local) }
func (s *State) Result() *Result {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.result == nil {
		return nil
	}
	r := *s.result
	return &r
}
func (s *State) Finish(outbound, family string, connected bool, code string, lookups []dns.LookupTrace) {
	complete := len(lookups) > 0 && len(lookups) <= 8
	for _, lookup := range lookups {
		complete = complete && lookup.Complete
	}
	node := nodeIdentity()
	if !Identifier(node, 128) {
		node = ""
		complete = false
	}
	var profile struct {
		Fingerprint string            `json:"fingerprint"`
		Strategies  map[string]string `json:"strategies"`
	}
	encoded := os.Getenv(ProfileEnv)
	if len(encoded) > 65536 || json.Unmarshal([]byte(encoded), &profile) != nil {
		complete = false
	}
	normalTag := strings.TrimSuffix(strings.TrimSuffix(outbound, ":probe-v4"), ":probe-v6")
	normalStrategy := profile.Strategies[normalTag]
	if normalStrategy == "" || len(profile.Fingerprint) != 64 {
		complete = false
	}
	s.Set(Result{Exit: node, Outbound: outbound, Profile: profile.Fingerprint, NormalStrategy: normalStrategy, Family: family, Connected: connected, Complete: complete, Error: code, Lookups: lookups})
}

func nodeIdentity() string {
	if path := os.Getenv(NodeFileEnv); path != "" {
		file, err := os.Open(path)
		if err != nil {
			return ""
		}
		defer file.Close()
		info, err := file.Stat()
		if err != nil || !info.Mode().IsRegular() || info.Mode().Perm()&0077 != 0 || info.Size() > 128 {
			return ""
		}
		data, err := io.ReadAll(io.LimitReader(file, 129))
		if err != nil {
			return ""
		}
		return string(data)
	}
	return ""
}
func probeUser(ctx context.Context) bool {
	inbound := session.InboundFromContext(ctx)
	return inbound != nil && inbound.User != nil && (strings.HasPrefix(inbound.User.Email, "probe#v4:") || strings.HasPrefix(inbound.User.Email, "probe#v6:"))
}
func localGroup(ctx context.Context) string {
	inbound := session.InboundFromContext(ctx)
	if inbound == nil || !strings.HasPrefix(inbound.Tag, LocalTag) || os.Getenv(FileEnv) == "" {
		return ""
	}
	group := strings.TrimPrefix(inbound.Tag, LocalTag)
	if !Identifier(group, 96) {
		return ""
	}
	return group
}
func newLocal(domain, group string) (*State, error) {
	var id [16]byte
	if _, err := rand.Read(id[:]); err != nil {
		return nil, err
	}
	state := &State{ID: hex.EncodeToString(id[:]), Domain: domain, local: true}
	// Emit before the stream can connect. A late completion then cannot move into
	// the next HTTP attempt merely because its file write happened after that attempt began.
	_ = export(Record{Group: group, Started: true, Result: Result{Version: Version, ID: state.ID, Domain: domain, Error: "pending", Lookups: make([]dns.LookupTrace, 0)}})
	state.emit = func(result Result) error { return export(Record{Group: group, Result: result}) }
	return state, nil
}

var exportMu sync.Mutex
var exported int

func export(record Record) error {
	encoded, err := json.Marshal(record)
	if err != nil {
		return err
	}
	encoded = append(encoded, '\n')
	exportMu.Lock()
	defer exportMu.Unlock()
	if exported >= MaxRecords {
		return errors.New("probe evidence limit")
	}
	path := os.Getenv(FileEnv)
	info, err := os.Lstat(path)
	if err != nil {
		return err
	}
	if !info.Mode().IsRegular() || info.Mode().Perm()&0077 != 0 {
		return errors.New("unsafe probe evidence file")
	}
	file, err := os.OpenFile(path, os.O_WRONLY|os.O_APPEND, 0600)
	if err != nil {
		return err
	}
	defer file.Close()
	actual, err := file.Stat()
	if err != nil {
		return err
	}
	if !os.SameFile(info, actual) || actual.Size()+int64(len(encoded)) > MaxFile {
		return errors.New("probe evidence file limit")
	}
	_, err = file.Write(encoded)
	if err == nil {
		exported++
	}
	return err
}
func Family(address net.Address) string {
	if address == nil || !address.Family().IsIP() {
		return ""
	}
	if address.Family().IsIPv4() {
		return "v4"
	}
	return "v6"
}
