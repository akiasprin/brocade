package freedom

import (
	"context"
	"crypto/rand"
	"io"
	"strings"
	"sync/atomic"
	"time"

	"github.com/pires/go-proxyproto"
	"github.com/xtls/xray-core/app/dispatcher"
	"github.com/xtls/xray-core/common"
	"github.com/xtls/xray-core/common/buf"
	"github.com/xtls/xray-core/common/crypto"
	"github.com/xtls/xray-core/common/dice"
	"github.com/xtls/xray-core/common/errors"
	"github.com/xtls/xray-core/common/geodata"
	"github.com/xtls/xray-core/common/net"
	"github.com/xtls/xray-core/common/platform"
	"github.com/xtls/xray-core/common/retry"
	"github.com/xtls/xray-core/common/session"
	"github.com/xtls/xray-core/common/signal"
	"github.com/xtls/xray-core/common/task"
	"github.com/xtls/xray-core/common/utils"
	"github.com/xtls/xray-core/core"
	feature_dns "github.com/xtls/xray-core/features/dns"
	"github.com/xtls/xray-core/features/policy"
	"github.com/xtls/xray-core/features/probetrace"
	"github.com/xtls/xray-core/features/stats"
	"github.com/xtls/xray-core/proxy"
	"github.com/xtls/xray-core/transport"
	"github.com/xtls/xray-core/transport/internet"
	"github.com/xtls/xray-core/transport/internet/stat"
)

var useSplice atomic.Bool
var allNetworks = [8]bool{true, true, true, true, true, true, true, true}

func responseSpliceEnabled(inbound *session.Inbound) bool {
	return responseSpliceAllowed(useSplice.Load(), inbound)
}

func responseSpliceAllowed(enabled bool, inbound *session.Inbound) bool {
	if enabled {
		return true
	}
	// Brocade's legacy Agent disabled Freedom splice process-wide to protect TLS/REALITY
	// dokodemo fronts. A patched Xray can still honor that guard while allowing Vision,
	// whose state changes from waiting to direct only after it has identified an inner encrypted
	// stream that is safe to copy through the outer security layer.
	if inbound == nil || inbound.Name != "vless" {
		return false
	}
	state := inbound.CanSpliceCopy.Load()
	return state == session.SpliceCopyDirect || state == session.SpliceCopyWaiting
}

func init() {
	common.Must(common.RegisterConfig((*Config)(nil), func(ctx context.Context, config interface{}) (interface{}, error) {
		h := new(Handler)
		if stream, ok := session.StreamSettingsFromContext(ctx).(*internet.MemoryStreamConfig); ok && stream != nil && stream.SocketSettings != nil {
			h.resolveStrategy = stream.SocketSettings.DomainStrategy
			h.usesDialerProxy = stream.SocketSettings.DialerProxy != ""
		}
		if err := core.RequireFeatures(ctx, func(pm policy.Manager) error {
			return h.Init(config.(*Config), pm)
		}); err != nil {
			return nil, err
		}
		return h, nil
	}))
	platform.RegisterEnvReload(reloadEnvSettings)

}

type FinalRule struct {
	action     RuleAction
	network    [8]bool
	port       net.MemoryPortList
	ip         geodata.IPMatcher
	blockDelay *Range
}

func buildFinalRule(config *FinalRuleConfig) (*FinalRule, error) {
	rule := &FinalRule{
		action:     config.GetAction(),
		blockDelay: config.GetBlockDelay(),
	}

	if len(config.Networks) == 0 {
		rule.network = allNetworks
	} else {
		for _, network := range config.Networks {
			rule.network[int(network)] = true
		}
	}

	if config.PortList != nil {
		rule.port = net.PortListFromProto(config.PortList)
	}

	if len(config.Ip) > 0 {
		matcher, err := geodata.IPReg.BuildIPMatcher(config.Ip)
		if err != nil {
			return nil, err
		}
		rule.ip = matcher
	}

	return rule, nil
}

func (r *FinalRule) matchNetwork(network net.Network) bool {
	return r.network[int(network)]
}

func (r *FinalRule) matchPort(port net.Port) bool {
	if len(r.port) == 0 {
		return true
	}
	return r.port.Contains(port)
}

func (r *FinalRule) matchIP(addr net.Address) bool {
	if r.ip == nil {
		return true
	}
	return addr != nil && addr.Family().IsIP() && r.ip.Match(addr.IP())
}

func (r *FinalRule) Apply(network net.Network, address net.Address, port net.Port) bool {
	if !r.matchNetwork(network) {
		return false
	}
	if !r.matchPort(port) {
		return false
	}
	return r.matchIP(address)
}

func (h *Handler) matchFinalRule(network net.Network, address net.Address, port net.Port, defaultRule *FinalRule) *FinalRule {
	for _, rule := range h.finalRules {
		if rule.Apply(network, address, port) {
			return rule
		}
	}
	if defaultRule != nil && defaultRule.Apply(network, address, port) {
		return defaultRule
	}
	return nil
}

func (h *Handler) blockDelay(rule *FinalRule) time.Duration {
	min := uint64(30)
	max := uint64(90)
	if rule.blockDelay != nil {
		min = rule.blockDelay.Min
		max = rule.blockDelay.Max
	}
	span := max - min
	if max < min {
		span = min - max
	}
	return time.Duration(min+uint64(dice.Roll(int(span+1)))) * time.Second
}

func (h *Handler) blackhole(ctx context.Context, input buf.Reader, output buf.Writer, rule *FinalRule, dest *net.Destination) error {
	delay := h.blockDelay(rule)
	errors.LogInfo(ctx, "blocked target: ", *dest, ", blackholing connection for ", delay)
	timer := time.AfterFunc(delay, func() {
		common.Interrupt(input)
		common.Interrupt(output)
		errors.LogInfo(ctx, "closed blackholed connection to blocked target: ", *dest)
	})
	defer timer.Stop()
	defer common.Close(output)
	_ = buf.Copy(input, buf.Discard)
	return nil
}

func reloadEnvSettings() error {
	const defaultFlagValue = "NOT_DEFINED_AT_ALL"
	value := platform.NewEnvFlag(platform.UseFreedomSplice).GetValue(func() string { return defaultFlagValue })
	enabled := false
	switch value {
	case defaultFlagValue, "auto", "enable":
		enabled = true
	}
	useSplice.Store(enabled)
	return nil
}

// Handler handles Freedom connections.
type Handler struct {
	policyManager    policy.Manager
	config           *Config
	blockedIPMatcher ipMatcher
	finalRules       []*FinalRule
	resolveStrategy  internet.DomainStrategy
	usesDialerProxy  bool
}

// ProbeTraceTerminal distinguishes a real socket dial from a relay transport.
func (*Handler) ProbeTraceTerminal() {}

// Init initializes the Handler with necessary parameters.
func (h *Handler) Init(config *Config, pm policy.Manager) error {
	h.config = config
	h.policyManager = pm
	if h.usesDialerProxy { // freedom is not the final outbound, final rules do not apply
		if len(config.FinalRules) > 0 {
			errors.LogWarning(context.Background(), `The "finalRules" setting is ignored when "sockopt.dialerProxy" is set, since freedom is not the final outbound.`)
		}
		return nil
	}
	h.finalRules = make([]*FinalRule, 0, len(config.FinalRules))
	for _, rc := range config.FinalRules {
		rule, err := buildFinalRule(rc)
		if err != nil {
			return errors.New("failed to build final rule").Base(err)
		}
		h.finalRules = append(h.finalRules, rule)
	}
	return nil
}

func (h *Handler) policy() policy.Session {
	p := h.policyManager.ForLevel(h.config.UserLevel)
	return p
}

func isValidAddress(addr *net.IPOrDomain) bool {
	if addr == nil {
		return false
	}

	a := addr.AsAddress()
	return a != net.AnyIP && a != net.AnyIPv6
}

type ipMatcher interface{ Match(net.IP) bool }
type ipMatchFunc func(net.IP) bool

func (f ipMatchFunc) Match(ip net.IP) bool { return f(ip) }

func isBlockedAddress(matcher ipMatcher, addr net.Address) bool {
	return matcher != nil && addr != nil && addr.Family().IsIP() && matcher.Match(addr.IP())
}

// Process implements proxy.Outbound.
func (h *Handler) Process(ctx context.Context, link *transport.Link, dialer internet.Dialer) error {
	outbounds := session.OutboundsFromContext(ctx)
	ob := outbounds[len(outbounds)-1]
	if !ob.Target.IsValid() {
		return errors.New("target not specified.")
	}
	ob.Name = "freedom"
	ob.CanSpliceCopy.Store(session.SpliceCopyDirect)
	inbound := session.InboundFromContext(ctx)
	var blockedIPMatcher ipMatcher
	if h.blockedIPMatcher != nil {
		blockedIPMatcher = h.blockedIPMatcher
	}
	if inbound != nil && strings.HasPrefix(inbound.Tag, "fwd:") {
		if inbound.ForwardingTargetIPMatcher == nil {
			return errors.New("user forwarding has no target address policy")
		}
		forwardingMatcher := inbound.ForwardingTargetIPMatcher
		outboundMatcher := h.blockedIPMatcher
		blockedIPMatcher = ipMatchFunc(func(ip net.IP) bool {
			return forwardingMatcher.Match(ip) || (outboundMatcher != nil && outboundMatcher.Match(ip))
		})
	}
	if blockedIPMatcher != nil || (!h.usesDialerProxy && len(h.finalRules) > 0) {
		forwardingMatcher := blockedIPMatcher
		blockedIPMatcher = ipMatchFunc(func(ip net.IP) bool {
			return (forwardingMatcher != nil && forwardingMatcher.Match(ip)) || h.finalBlocked(ob.Target.Network, net.IPAddress(ip), ob.Target.Port)
		})
		ctx = internet.ContextWithIPBlocker(ctx, blockedIPMatcher.Match)

	}

	destination := ob.Target
	origTargetAddr := ob.OriginalTarget.Address
	if origTargetAddr == nil {
		origTargetAddr = ob.Target.Address
	}
	dialer.SetOutboundGateway(ctx, ob)
	outGateway := ob.Gateway
	UDPOverride := net.UDPDestination(nil, 0)
	if h.config.DestinationOverride != nil {
		server := h.config.DestinationOverride.Server
		if isValidAddress(server.Address) {
			destination.Address = server.Address.AsAddress()
			UDPOverride.Address = destination.Address
		}
		if server.Port != 0 {
			destination.Port = net.Port(server.Port)
			UDPOverride.Port = destination.Port
		}
	}

	input := link.Reader
	output := link.Writer

	var conn stat.Connection
	trace := probetrace.FromContext(ctx)
	var lookups []feature_dns.LookupTrace
	var tracedLookups *[]feature_dns.LookupTrace
	if trace != nil {
		tracedLookups = &lookups
	}
	traceCode := "connect_error"
	traceFamily := ""
	if trace != nil {
		defer func() { trace.Finish(ob.Tag, traceFamily, false, traceCode, lookups) }()
	}
	attempts := 5
	if trace != nil {
		attempts = 1
	}
	err := retry.ExponentialBackoff(attempts, 100).On(func() error {
		dialDest := destination
		if h.domainStrategy().HasStrategy() && dialDest.Address.Family().IsDomain() {
			strategy := h.domainStrategy()
			if destination.Network == net.Network_UDP && origTargetAddr != nil && outGateway == nil {
				strategy = strategy.GetDynamicStrategy(origTargetAddr.Family())
			}
			ips, err := internet.LookupForIPTraceContext(ctx, dialDest.Address.Domain(), strategy, outGateway, tracedLookups)
			if err != nil {
				traceCode = "dns_error"
				if errors.Cause(err) == feature_dns.ErrEmptyResponse {
					traceCode = "dns_empty"
				}
				errors.LogInfoInner(ctx, err, "failed to get IP address for domain ", dialDest.Address.Domain())
				if h.domainStrategy().ForceIP() {
					return err
				}
			} else {
				dialDest = net.Destination{
					Network: dialDest.Network,
					Address: net.IPAddress(ips[dice.Roll(len(ips))]),
					Port:    dialDest.Port,
				}
				errors.LogInfo(ctx, "dialing to ", dialDest)
			}
		}

		if isBlockedAddress(blockedIPMatcher, dialDest.Address) {
			return errors.New("blocked target IP: ", dialDest.Address)
		}
		if trace != nil {
			traceFamily = probetrace.Family(dialDest.Address)
		}
		rawConn, err := dialer.Dial(ctx, dialDest)
		if err != nil {
			if trace != nil {
				traceCode = internet.ProbeDialErrorCode(err)
			}
			return err
		}

		conn = rawConn
		if trace != nil {
			trace.Finish(ob.Tag, probetrace.Family(net.DestinationFromAddr(conn.RemoteAddr()).Address), true, "", lookups)
		}
		return nil
	})
	if err != nil {
		return errors.New("failed to open connection to ", destination).Base(err)
	}
	if remoteAddr := net.DestinationFromAddr(conn.RemoteAddr()).Address; isBlockedAddress(blockedIPMatcher, remoteAddr) {
		conn.Close()
		return errors.New("blocked target IP: ", remoteAddr)
	}
	if h.config.ProxyProtocol > 0 && h.config.ProxyProtocol <= 2 {
		version := byte(h.config.ProxyProtocol)
		srcAddr := inbound.Source.RawNetAddr()
		dstAddr := conn.RemoteAddr()
		header := proxyproto.HeaderProxyFromAddrs(version, srcAddr, dstAddr)
		if _, err = header.WriteTo(conn); err != nil {
			conn.Close()
			return errors.New("failed to set PROXY protocol v", version).Base(err)
		}
	}
	defer conn.Close()
	errors.LogInfo(ctx, "connection opened to ", destination, ", local endpoint ", conn.LocalAddr(), ", remote endpoint ", conn.RemoteAddr())

	var newCtx context.Context
	var newCancel context.CancelFunc
	if session.TimeoutOnlyFromContext(ctx) {
		newCtx, newCancel = context.WithCancel(context.Background())
	}

	plcy := h.policy()
	ctx, cancel := context.WithCancel(ctx)
	timer := signal.CancelAfterInactivity(ctx, func() {
		cancel()
		if newCancel != nil {
			newCancel()
		}
	}, plcy.Timeouts.ConnectionIdle)

	requestDone := func() error {
		defer timer.SetTimeout(plcy.Timeouts.DownlinkOnly)

		var writer buf.Writer
		if destination.Network == net.Network_TCP {
			if h.config.Fragment != nil {
				errors.LogDebug(ctx, "FRAGMENT", h.config.Fragment.PacketsFrom, h.config.Fragment.PacketsTo, h.config.Fragment.LengthMin, h.config.Fragment.LengthMax,
					h.config.Fragment.IntervalMin, h.config.Fragment.IntervalMax, h.config.Fragment.MaxSplitMin, h.config.Fragment.MaxSplitMax)
				writer = buf.NewWriter(&FragmentWriter{
					fragment: h.config.Fragment,
					writer:   conn,
				})
			} else {
				writer = buf.NewWriter(conn)
			}
		} else {
			writer = NewPacketWriter(conn, h, UDPOverride, destination, blockedIPMatcher)
			if h.config.Noises != nil {
				errors.LogDebug(ctx, "NOISE", h.config.Noises)
				writer = &NoisePacketWriter{
					Writer:      writer,
					noises:      h.config.Noises,
					firstWrite:  true,
					UDPOverride: UDPOverride,
					remoteAddr:  net.DestinationFromAddr(conn.RemoteAddr()).Address,
				}
			}
		}

		if err := buf.Copy(input, writer, buf.UpdateActivity(timer)); err != nil {
			return errors.New("failed to process request").Base(err)
		}

		return nil
	}

	responseDone := func() error {
		defer timer.SetTimeout(plcy.Timeouts.UplinkOnly)
		if trace == nil && destination.Network == net.Network_TCP && inbound != nil && inbound.FramedDownlinkSplicer != nil &&
			responseSpliceEnabled(inbound) && proxy.IsRAWTransportWithoutSecurity(conn) {
			rawConn, readCounter, _ := proxy.UnwrapRawConn(conn)
			var userCounter stats.Counter
			if statWriter, ok := output.(*dispatcher.SizeStatWriter); ok {
				userCounter = statWriter.Counter
			}
			handled, spliceErr := inbound.FramedDownlinkSplicer.SpliceDownlink(ctx, rawConn, func(bytes int64) {
				if readCounter != nil {
					readCounter.Add(bytes)
				}
				if userCounter != nil {
					userCounter.Add(bytes)
				}
				timer.Update()
			})
			if handled {
				if spliceErr != nil && errors.Cause(spliceErr) != io.EOF {
					return errors.New("failed to splice framed response").Base(spliceErr)
				}
				return nil
			}
		}
		if destination.Network != net.Network_TCP {
			proxy.SetSpliceNotUsedReason(inbound, session.SpliceNotUsedUnsupportedCommand)
		} else if !responseSpliceEnabled(inbound) {
			proxy.SetSpliceNotUsedReason(inbound, session.SpliceNotUsedGloballyDisabled)
		} else if !proxy.IsRAWTransportWithoutSecurity(conn) {
			proxy.SetSpliceNotUsedReason(inbound, session.SpliceNotUsedOutboundNotRaw)
		} else { // it would be tls conn in special use case of MITM, we need to let link handle traffic
			var writeConn net.Conn
			var inTimer *signal.ActivityTimer
			if inbound := session.InboundFromContext(ctx); inbound != nil && inbound.Conn != nil {
				writeConn = inbound.Conn
				inTimer = inbound.Timer
			}
			return proxy.CopyRawConnIfExist(ctx, conn, writeConn, link.Writer, timer, inTimer)
		}
		var reader buf.Reader
		if destination.Network == net.Network_TCP {
			reader = buf.NewReader(conn)
		} else {
			reader = NewPacketReader(conn, UDPOverride, destination, blockedIPMatcher, h)
		}
		if err := buf.Copy(reader, output, buf.UpdateActivity(timer)); err != nil {
			return errors.New("failed to process response").Base(err)
		}
		return nil
	}

	if newCtx != nil {
		ctx = newCtx
	}

	if err := task.Run(ctx, requestDone, task.OnSuccess(responseDone, task.Close(output))); err != nil {
		return errors.New("connection ends").Base(err)
	}

	return nil
}

func NewPacketReader(conn net.Conn, UDPOverride net.Destination, DialDest net.Destination, blockedIPMatcher ipMatcher, handlers ...*Handler) buf.Reader {
	var handler *Handler
	if len(handlers) > 0 {
		handler = handlers[0]
	}
	iConn := conn
	statConn, ok := iConn.(*stat.CounterConnection)
	if ok {
		iConn = statConn.Connection
	}
	var counter stats.Counter
	if statConn != nil {
		counter = statConn.ReadCounter
	}
	if c, ok := iConn.(*internet.PacketConnWrapper); ok {
		isOverridden := false
		if UDPOverride.Address != nil || UDPOverride.Port != 0 {
			isOverridden = true
		}

		return &PacketReader{
			PacketConnWrapper: c,
			Counter:           counter,
			BlockedIPMatcher:  blockedIPMatcher,
			Handler:           handler,
			IsOverridden:      isOverridden,
			InitUnchangedAddr: DialDest.Address,
			InitChangedAddr:   net.DestinationFromAddr(conn.RemoteAddr()).Address,
		}
	}
	return &buf.PacketReader{Reader: conn}
}

type PacketReader struct {
	Handler *Handler
	*internet.PacketConnWrapper
	stats.Counter
	BlockedIPMatcher  ipMatcher
	IsOverridden      bool
	InitUnchangedAddr net.Address
	InitChangedAddr   net.Address
}

func (r *PacketReader) ReadMultiBuffer() (buf.MultiBuffer, error) {
	b := buf.NewWithSize(buf.MaxPacketSize)
	b.Resize(0, buf.MaxPacketSize)
	for {
		n, d, err := r.PacketConnWrapper.ReadFrom(b.Bytes())
		if err != nil {
			b.Release()
			return nil, err
		}
		udpAddr := d.(*net.UDPAddr)
		sourceAddr := net.IPAddress(udpAddr.IP)
		if isBlockedAddress(r.BlockedIPMatcher, sourceAddr) || r.Handler.finalBlocked(net.Network_UDP, sourceAddr, net.Port(udpAddr.Port)) {
			continue
		}
		b.Resize(0, int32(n))

		// if udp dest addr is changed, we are unable to get the correct src addr
		// so we don't attach src info to udp packet, break cone behavior, assuming the dial dest is the expected scr addr
		if !r.IsOverridden {
			if r.InitChangedAddr == sourceAddr {
				sourceAddr = r.InitUnchangedAddr
			}
			b.UDP = &net.Destination{
				Address: sourceAddr,
				Port:    net.Port(udpAddr.Port),
				Network: net.Network_UDP,
			}
		}
		if r.Counter != nil {
			r.Counter.Add(int64(n))
		}
		return buf.MultiBuffer{b}, nil
	}
}

// DialDest means the dial target used in the dialer when creating conn
func NewPacketWriter(conn net.Conn, h *Handler, UDPOverride net.Destination, DialDest net.Destination, blockedIPMatcher ipMatcher) buf.Writer {
	iConn := conn
	statConn, ok := iConn.(*stat.CounterConnection)
	if ok {
		iConn = statConn.Connection
	}
	var counter stats.Counter
	if statConn != nil {
		counter = statConn.WriteCounter
	}
	if c, ok := iConn.(*internet.PacketConnWrapper); ok {
		// If DialDest is a domain, it will be resolved in dialer
		// check this behavior and add it to map
		resolvedUDPAddr := utils.NewTypedSyncMap[string, net.Address]()
		if DialDest.Address.Family().IsDomain() {
			resolvedUDPAddr.Store(DialDest.Address.Domain(), net.DestinationFromAddr(conn.RemoteAddr()).Address)
		}
		return &PacketWriter{
			PacketConnWrapper: c,
			Counter:           counter,
			Handler:           h,
			BlockedIPMatcher:  blockedIPMatcher,
			UDPOverride:       UDPOverride,
			ResolvedUDPAddr:   resolvedUDPAddr,
			LocalAddr:         net.DestinationFromAddr(conn.LocalAddr()).Address,
		}

	}
	return &buf.SequentialWriter{Writer: conn}
}

type PacketWriter struct {
	*internet.PacketConnWrapper
	stats.Counter
	*Handler
	BlockedIPMatcher ipMatcher
	UDPOverride      net.Destination

	// Dest of udp packets might be a domain, we will resolve them to IP
	// But resolver will return a random one if the domain has many IPs
	// Resulting in these packets being sent to many different IPs randomly
	// So, cache and keep the resolve result
	ResolvedUDPAddr *utils.TypedSyncMap[string, net.Address]
	LocalAddr       net.Address
}

func (w *PacketWriter) WriteMultiBuffer(mb buf.MultiBuffer) error {
	for {
		mb2, b := buf.SplitFirst(mb)
		mb = mb2
		if b == nil {
			break
		}
		var n int
		var err error
		if b.UDP != nil {
			if w.UDPOverride.Address != nil {
				b.UDP.Address = w.UDPOverride.Address
			}
			if w.UDPOverride.Port != 0 {
				b.UDP.Port = w.UDPOverride.Port
			}
			if b.UDP.Address.Family().IsDomain() {
				if ip, ok := w.ResolvedUDPAddr.Load(b.UDP.Address.Domain()); ok {
					b.UDP.Address = ip
				} else {
					ShouldUseSystemResolver := true
					if w.Handler.domainStrategy().HasStrategy() {
						ips, err := internet.LookupForIP(b.UDP.Address.Domain(), w.Handler.domainStrategy(), w.LocalAddr)
						if err != nil {
							// drop packet if resolve failed when forceIP
							if w.Handler.domainStrategy().ForceIP() {
								b.Release()
								continue
							}
						} else {
							ip = net.IPAddress(ips[dice.Roll(len(ips))])
							ShouldUseSystemResolver = false
						}
					}
					if ShouldUseSystemResolver {
						udpAddr, err := net.ResolveUDPAddr("udp", b.UDP.NetAddr())
						if err != nil {
							b.Release()
							continue
						} else {
							ip = net.IPAddress(udpAddr.IP)
						}
					}
					if ip != nil {
						b.UDP.Address, _ = w.ResolvedUDPAddr.LoadOrStore(b.UDP.Address.Domain(), ip)
					}
				}
			}
			if isBlockedAddress(w.BlockedIPMatcher, b.UDP.Address) || w.Handler.finalBlocked(net.Network_UDP, b.UDP.Address, b.UDP.Port) {
				b.Release()
				continue
			}
			destAddr := b.UDP.RawNetAddr()
			if destAddr == nil {
				b.Release()
				continue
			}
			n, err = w.PacketConnWrapper.WriteTo(b.Bytes(), destAddr)
		} else {
			n, err = w.PacketConnWrapper.Write(b.Bytes())
		}
		b.Release()
		if err != nil {
			buf.ReleaseMulti(mb)
			return err
		}
		if w.Counter != nil {
			w.Counter.Add(int64(n))
		}
	}
	return nil
}

type NoisePacketWriter struct {
	buf.Writer
	noises      []*Noise
	firstWrite  bool
	UDPOverride net.Destination
	remoteAddr  net.Address
}

// MultiBuffer writer with Noise before first packet
func (w *NoisePacketWriter) WriteMultiBuffer(mb buf.MultiBuffer) error {
	if w.firstWrite {
		w.firstWrite = false
		//Do not send Noise for dns requests(just to be safe)
		if w.UDPOverride.Port == 53 {
			return w.Writer.WriteMultiBuffer(mb)
		}
		var noise []byte
		var err error
		if w.remoteAddr.Family().IsDomain() {
			panic("impossible, remoteAddr is always IP")
		}
		for _, n := range w.noises {
			switch n.ApplyTo {
			case "ipv4":
				if w.remoteAddr.Family().IsIPv6() {
					continue
				}
			case "ipv6":
				if w.remoteAddr.Family().IsIPv4() {
					continue
				}
			case "ip":
			default:
				panic("unreachable, applyTo is ip/ipv4/ipv6")
			}
			//User input string or base64 encoded string or hex string
			if n.Packet != nil {
				noise = n.Packet
			} else {
				//Random noise
				noise, err = GenerateRandomBytes(crypto.RandBetween(int64(n.LengthMin),
					int64(n.LengthMax)))
			}
			if err != nil {
				return err
			}
			err = w.Writer.WriteMultiBuffer(buf.MultiBuffer{buf.FromBytes(noise)})
			if err != nil {
				return err
			}

			if n.DelayMin != 0 || n.DelayMax != 0 {
				time.Sleep(time.Duration(crypto.RandBetween(int64(n.DelayMin), int64(n.DelayMax))) * time.Millisecond)
			}
		}

	}
	return w.Writer.WriteMultiBuffer(mb)
}

type FragmentWriter struct {
	fragment *Fragment
	writer   io.Writer
	count    uint64
}

func (f *FragmentWriter) Write(b []byte) (int, error) {
	f.count++

	if f.fragment.PacketsFrom == 0 && f.fragment.PacketsTo == 1 {
		if f.count != 1 || len(b) <= 5 || b[0] != 22 {
			return f.writer.Write(b)
		}
		recordLen := 5 + ((int(b[3]) << 8) | int(b[4]))
		if len(b) < recordLen { // maybe already fragmented somehow
			return f.writer.Write(b)
		}
		data := b[5:recordLen]
		buff := make([]byte, 2048)
		var hello []byte
		maxSplit := crypto.RandBetween(int64(f.fragment.MaxSplitMin), int64(f.fragment.MaxSplitMax))
		var splitNum int64
		for from := 0; ; {
			to := from + int(crypto.RandBetween(int64(f.fragment.LengthMin), int64(f.fragment.LengthMax)))
			splitNum++
			if to > len(data) || (maxSplit > 0 && splitNum >= maxSplit) {
				to = len(data)
			}
			l := to - from
			if 5+l > len(buff) {
				buff = make([]byte, 5+l)
			}
			copy(buff[:3], b)
			copy(buff[5:], data[from:to])
			from = to
			buff[3] = byte(l >> 8)
			buff[4] = byte(l)
			if f.fragment.IntervalMax == 0 { // combine fragmented tlshello if interval is 0
				hello = append(hello, buff[:5+l]...)
			} else {
				_, err := f.writer.Write(buff[:5+l])
				time.Sleep(time.Duration(crypto.RandBetween(int64(f.fragment.IntervalMin), int64(f.fragment.IntervalMax))) * time.Millisecond)
				if err != nil {
					return 0, err
				}
			}
			if from == len(data) {
				if len(hello) > 0 {
					_, err := f.writer.Write(hello)
					if err != nil {
						return 0, err
					}
				}
				if len(b) > recordLen {
					n, err := f.writer.Write(b[recordLen:])
					if err != nil {
						return recordLen + n, err
					}
				}
				return len(b), nil
			}
		}
	}

	if f.fragment.PacketsFrom != 0 && (f.count < f.fragment.PacketsFrom || f.count > f.fragment.PacketsTo) {
		return f.writer.Write(b)
	}
	maxSplit := crypto.RandBetween(int64(f.fragment.MaxSplitMin), int64(f.fragment.MaxSplitMax))
	var splitNum int64
	for from := 0; ; {
		to := from + int(crypto.RandBetween(int64(f.fragment.LengthMin), int64(f.fragment.LengthMax)))
		splitNum++
		if to > len(b) || (maxSplit > 0 && splitNum >= maxSplit) {
			to = len(b)
		}
		n, err := f.writer.Write(b[from:to])
		from += n
		if err != nil {
			return from, err
		}
		time.Sleep(time.Duration(crypto.RandBetween(int64(f.fragment.IntervalMin), int64(f.fragment.IntervalMax))) * time.Millisecond)
		if from >= len(b) {
			return from, nil
		}
	}
}

func GenerateRandomBytes(n int64) ([]byte, error) {
	b := make([]byte, n)
	_, err := rand.Read(b)
	// Note that err == nil only if we read len(b) bytes.
	if err != nil {
		return nil, err
	}

	return b, nil
}

func (h *Handler) domainStrategy() internet.DomainStrategy {
	if h.resolveStrategy.HasStrategy() {
		return h.resolveStrategy
	}
	return h.config.DomainStrategy
}
func (h *Handler) finalBlocked(network net.Network, address net.Address, port net.Port) bool {
	if h == nil || h.usesDialerProxy {
		return false
	}
	rule := h.matchFinalRule(network, address, port, nil)
	return rule != nil && rule.action == RuleAction_Block
}
