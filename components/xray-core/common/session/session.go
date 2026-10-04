// Package session provides functions for sessions of incoming requests.
package session // import "github.com/xtls/xray-core/common/session"

import (
	"context"
	"math/rand"
	"sync/atomic"

	c "github.com/xtls/xray-core/common/ctx"
	"github.com/xtls/xray-core/common/errors"
	"github.com/xtls/xray-core/common/geodata"
	"github.com/xtls/xray-core/common/net"
	"github.com/xtls/xray-core/common/protocol"
	"github.com/xtls/xray-core/common/signal"
)

// FramedDownlinkSplicer allows a multiplexed inbound to preserve its frame
// boundaries while moving payload bytes directly from a raw outbound socket,
// bypassing the ordinary transport-pipe copy loop.
// Implementations must return handled=false before consuming any source bytes
// when the fast path is unavailable.
type FramedDownlinkSplicer interface {
	SpliceDownlink(context.Context, net.Conn, func(int64)) (handled bool, err error)
}

// SpliceCopyState describes the connection's progression from protocol parsing
// through direct copy and into an operating-system-assisted splice. The zero
// value deliberately means that no protocol has opted the connection in.
type SpliceCopyState int32

const (
	SpliceCopyUnknown SpliceCopyState = iota
	SpliceCopyDirect
	SpliceCopyWaiting
	SpliceCopyDisabled
	SpliceCopySplicing
)

// AtomicSpliceCopyState synchronizes the protocol and copy goroutines that
// negotiate the transition to splice. Keep the underlying value private so new
// call sites cannot accidentally reintroduce unsynchronized access.
type AtomicSpliceCopyState struct {
	value atomic.Int32
}

func (s *AtomicSpliceCopyState) Load() SpliceCopyState {
	return SpliceCopyState(s.value.Load())
}

func (s *AtomicSpliceCopyState) Store(state SpliceCopyState) {
	s.value.Store(int32(state))
}

func (s *AtomicSpliceCopyState) CompareAndSwap(old, new SpliceCopyState) bool {
	return s.value.CompareAndSwap(int32(old), int32(new))
}

// SpliceNotUsedReason is the terminal reason a splice-eligible protocol
// connection completed without entering splice.
type SpliceNotUsedReason uint8

const (
	SpliceNotUsedUnknown SpliceNotUsedReason = iota
	SpliceNotUsedUnsupportedCommand
	SpliceNotUsedUnsupportedTransport
	SpliceNotUsedOuterTLSNot13
	SpliceNotUsedGloballyDisabled
	SpliceNotUsedOutboundNotRaw
	SpliceNotUsedInboundIneligible
	SpliceNotUsedMissingOutbound
	SpliceNotUsedOutboundIneligible
	SpliceNotUsedRawConnectionUnavailable
	SpliceNotUsedEndedBeforeDirect
	SpliceNotUsedEndedBeforeSplice
)

// SpliceMetrics receives connection-level transitions. Implementations must be
// safe for concurrent calls from protocol and copy goroutines.
type SpliceMetrics interface {
	MarkDirect()
	AddDirectBytes(int64)
	MarkSplice()
	AddSpliceBytes(int64)
	SetNotSplicedReason(SpliceNotUsedReason)
	Finish()
}

// NewID generates a new ID. The generated ID is high likely to be unique, but not cryptographically secure.
// The generated ID will never be 0.
func NewID() c.ID {
	for {
		id := c.ID(rand.Uint32())
		if id != 0 {
			return id
		}
	}
}

// ExportIDToError transfers session.ID into an error object, for logging purpose.
// This can be used with error.WriteToLog().
func ExportIDToError(ctx context.Context) errors.ExportOption {
	id := c.IDFromContext(ctx)
	return func(h *errors.ExportOptionHolder) {
		h.SessionID = uint32(id)
	}
}

// Inbound is the metadata of an inbound connection.
type Inbound struct {
	// Source address of the inbound connection.
	Source net.Destination
	// Local address of the inbound connection.
	Local net.Destination
	// Gateway address.
	Gateway net.Destination
	// Tag of the inbound proxy that handles the connection.
	Tag string
	// Name of the inbound proxy that handles the connection.
	Name string
	// User is the user that authenticates for the inbound. May be nil if the protocol allows anonymous traffic.
	User *protocol.MemoryUser
	// VlessRoute is the user-sent VLESS UUID's 7th<<8 | 8th bytes.
	VlessRoute net.Port
	// Used by splice copy. Conn is actually internet.Connection. May be nil.
	Conn net.Conn
	// Used by splice copy. Timer of the inbound buf copier. May be nil.
	Timer *signal.ActivityTimer
	// CanSpliceCopy is the synchronized splice state for this connection.
	CanSpliceCopy AtomicSpliceCopyState
	// SpliceMetrics is set only by protocols that expose splice observability.
	SpliceMetrics SpliceMetrics
	// FramedDownlinkSplicer is set on per-stream metadata by multiplexed
	// protocols that can encode frames around an OS-assisted payload copy.
	FramedDownlinkSplicer FramedDownlinkSplicer
}

// Clone returns a shallow metadata copy while loading the atomic splice state
// safely. Mutable splice metrics intentionally stay attached to the logical
// connection unless the caller explicitly clears them.
func (i *Inbound) Clone() *Inbound {
	if i == nil {
		return nil
	}
	clone := &Inbound{
		Source:                i.Source,
		Local:                 i.Local,
		Gateway:               i.Gateway,
		Tag:                   i.Tag,
		Name:                  i.Name,
		User:                  i.User,
		VlessRoute:            i.VlessRoute,
		Conn:                  i.Conn,
		Timer:                 i.Timer,
		SpliceMetrics:         i.SpliceMetrics,
		FramedDownlinkSplicer: i.FramedDownlinkSplicer,
	}
	clone.CanSpliceCopy.Store(i.CanSpliceCopy.Load())
	return clone
}

// Outbound is the metadata of an outbound connection.
type Outbound struct {
	// Target address of the outbound connection.
	OriginalTarget net.Destination
	Target         net.Destination
	RouteTarget    net.Destination
	// Gateway address
	Gateway net.Address
	// Tag of the outbound proxy that handles the connection.
	Tag string
	// Name of the outbound proxy that handles the connection.
	Name string
	// Unused. Conn is actually internet.Connection. May be nil. It is currently nil for outbound with proxySettings
	Conn net.Conn
	// CanSpliceCopy is the synchronized splice state for this connection.
	CanSpliceCopy AtomicSpliceCopyState
}

// SniffingRequest controls the behavior of content sniffing. They are from inbound config. Read-only
type SniffingRequest struct {
	ExcludeForDomain               geodata.DomainMatcher
	ExcludeForIP                   geodata.IPMatcher
	OverrideDestinationForProtocol []string
	Enabled                        bool
	MetadataOnly                   bool
	RouteOnly                      bool
}

// Content is the metadata of the connection content. Mainly used for routing.
type Content struct {
	// Protocol of current content.
	Protocol string

	SniffingRequest SniffingRequest

	// HTTP traffic sniffed headers
	Attributes map[string]string

	// SkipDNSResolve is set from DNS module. the DOH remote server maybe a domain name, this prevents cycle resolving dead loop
	SkipDNSResolve bool
}

// Sockopt is the settings for socket connection.
type Sockopt struct {
	// Mark of the socket connection.
	Mark int32
}

// SetAttribute attaches additional string attributes to content.
func (c *Content) SetAttribute(name string, value string) {
	if c.Attributes == nil {
		c.Attributes = make(map[string]string)
	}
	c.Attributes[name] = value
}

// Attribute retrieves additional string attributes from content.
func (c *Content) Attribute(name string) string {
	if c.Attributes == nil {
		return ""
	}
	return c.Attributes[name]
}
