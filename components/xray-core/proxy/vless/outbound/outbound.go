package outbound

import (
	"bytes"
	"context"
	gotls "crypto/tls"
	"encoding/base64"
	"reflect"
	"strings"
	"sync"
	"time"
	"unsafe"

	utls "github.com/refraction-networking/utls"
	proxymanConfig "github.com/xtls/xray-core/app/proxyman"
	proxyman "github.com/xtls/xray-core/app/proxyman/outbound"
	"github.com/xtls/xray-core/app/reverse"
	"github.com/xtls/xray-core/common"
	"github.com/xtls/xray-core/common/buf"
	xctx "github.com/xtls/xray-core/common/ctx"
	"github.com/xtls/xray-core/common/errors"
	"github.com/xtls/xray-core/common/mux"
	"github.com/xtls/xray-core/common/net"
	"github.com/xtls/xray-core/common/protocol"
	"github.com/xtls/xray-core/common/retry"
	"github.com/xtls/xray-core/common/session"
	"github.com/xtls/xray-core/common/signal"
	"github.com/xtls/xray-core/common/task"
	"github.com/xtls/xray-core/common/xudp"
	"github.com/xtls/xray-core/core"
	"github.com/xtls/xray-core/features/policy"
	"github.com/xtls/xray-core/features/routing"
	"github.com/xtls/xray-core/proxy"
	"github.com/xtls/xray-core/proxy/vless"
	"github.com/xtls/xray-core/proxy/vless/encoding"
	"github.com/xtls/xray-core/proxy/vless/encryption"
	"github.com/xtls/xray-core/transport"
	"github.com/xtls/xray-core/transport/internet"
	"github.com/xtls/xray-core/transport/internet/reality"
	"github.com/xtls/xray-core/transport/internet/stat"
	"github.com/xtls/xray-core/transport/internet/tls"
	"github.com/xtls/xray-core/transport/pipe"
)

func init() {
	common.Must(common.RegisterConfig((*Config)(nil), func(ctx context.Context, config interface{}) (interface{}, error) {
		return New(ctx, config.(*Config))
	}))
}

// Handler is an outbound connection handler for VLess protocol.
type Handler struct {
	server        *protocol.ServerSpec
	policyManager policy.Manager
	cone          bool
	encryption    *encryption.ClientInstance
	reverse       *Reverse

	testpre  uint32
	initpre  sync.Once
	preConns chan *ConnExpire
}

type ConnExpire struct {
	Conn   stat.Connection
	Expire time.Time
}

func requestCommandForTarget(target net.Destination) (protocol.RequestCommand, error) {
	command := protocol.RequestCommandTCP
	if target.Network == net.Network_UDP {
		command = protocol.RequestCommandUDP
	}
	if target.Address == nil || !target.Address.Family().IsDomain() {
		return command, nil
	}
	switch target.Address.Domain() {
	case "v1.mux.cool":
		return protocol.RequestCommandMux, nil
	case "v1.rvs.cool":
		if target.Network != net.Network_Unknown {
			return 0, errors.New("nice try baby").AtError()
		}
		return protocol.RequestCommandRvs, nil
	default:
		return command, nil
	}
}

func dialVLESSServer(ctx context.Context, dialer internet.Dialer, destination net.Destination, command protocol.RequestCommand) (stat.Connection, error) {
	if command == protocol.RequestCommandRvs {
		// The reverse pool owns retries, concurrency and backoff. One pool attempt
		// must therefore correspond to exactly one physical dial.
		return dialer.Dial(ctx, destination)
	}
	var conn stat.Connection
	err := retry.ExponentialBackoff(5, 200).On(func() error {
		var err error
		conn, err = dialer.Dial(ctx, destination)
		return err
	})
	return conn, err
}

func requestActivityTimer(ctx context.Context, command protocol.RequestCommand, onTimeout context.CancelFunc, timeout time.Duration) *signal.ActivityTimer {
	if command == protocol.RequestCommandRvs {
		// Reverse health owns carrier liveness and drain. Per-user TCP idle and
		// half-close policy must not shorten that lifecycle.
		return signal.NewNoopActivityTimer()
	}
	return signal.CancelAfterInactivity(ctx, onTimeout, timeout)
}

// New creates a new VLess outbound handler.
func New(ctx context.Context, config *Config) (*Handler, error) {
	if config.Vnext == nil {
		return nil, errors.New(`no vnext found`)
	}
	server, err := protocol.NewServerSpecFromPB(config.Vnext)
	if err != nil {
		return nil, errors.New("failed to get server spec").Base(err).AtError()
	}

	v := core.MustFromContext(ctx)
	handler := &Handler{
		server:        server,
		policyManager: v.GetFeature(policy.ManagerType()).(policy.Manager),
		cone:          ctx.Value("cone").(bool),
	}

	a := handler.server.User.Account.(*vless.MemoryAccount)
	if a.Encryption != "" && a.Encryption != "none" {
		s := strings.Split(a.Encryption, ".")
		var nfsPKeysBytes [][]byte
		for _, r := range s {
			b, _ := base64.RawURLEncoding.DecodeString(r)
			nfsPKeysBytes = append(nfsPKeysBytes, b)
		}
		handler.encryption = &encryption.ClientInstance{}
		if err := handler.encryption.Init(nfsPKeysBytes, a.XorMode, a.Seconds, a.Padding); err != nil {
			return nil, errors.New("failed to use encryption").Base(err).AtError()
		}
	}

	handler.testpre = a.Testpre
	if a.Reverse != nil {
		rvsCtx := session.ContextWithInbound(ctx, &session.Inbound{
			Tag:  a.Reverse.Tag,
			Name: "vless-reverse",
			User: handler.server.User, // TODO: email
		})
		if sc := a.Reverse.Sniffing; sc != nil && sc.Enabled {
			request, err := proxymanConfig.BuildSniffingRequest(sc)
			if err != nil {
				return nil, errors.New("failed to build reverse sniffing request").Base(err).AtError()
			}
			rvsCtx = session.ContextWithContent(rvsCtx, &session.Content{
				SniffingRequest: request,
			})
		}
		hc, err := a.Reverse.HealthConfig("bridge")
		if err != nil {
			return nil, err
		}
		handler.reverse = &Reverse{health: hc,
			tag:        a.Reverse.Tag,
			dispatcher: v.GetFeature(routing.DispatcherType()).(routing.Dispatcher),
			ctx:        rvsCtx,
			handler:    handler,
		}
		handler.reverse.ctx, handler.reverse.cancel = context.WithCancel(rvsCtx)
		handler.reverse.wake = make(chan struct{}, 1)
		handler.reverse.reportDialDemand(0, 0, false)
		go handler.reverse.Start()
	}

	return handler, nil
}

// Close implements common.Closable.Close().
func (h *Handler) Close() error {
	if h.preConns != nil {
		close(h.preConns)
	}
	if h.reverse != nil {
		return h.reverse.Close()
	}
	return nil
}

// Process implements proxy.Outbound.Process().
func (h *Handler) Process(ctx context.Context, link *transport.Link, dialer internet.Dialer) error {
	outbounds := session.OutboundsFromContext(ctx)
	ob := outbounds[len(outbounds)-1]
	target := ob.Target
	command, err := requestCommandForTarget(target)
	if err != nil {
		return err
	}
	if !target.IsValid() && command != protocol.RequestCommandRvs {
		return errors.New("target not specified").AtError()
	}
	ob.Name = "vless"

	rec := h.server
	var conn stat.Connection

	if h.testpre > 0 && h.reverse == nil {
		h.initpre.Do(func() {
			h.preConns = make(chan *ConnExpire)
			for range h.testpre { // TODO: randomize
				go func() {
					defer func() { recover() }()
					ctx := xctx.ContextWithID(context.Background(), session.NewID())
					for {
						conn, err := dialer.Dial(ctx, rec.Destination)
						if err != nil {
							errors.LogWarningInner(ctx, err, "pre-connect failed")
							continue
						}
						h.preConns <- &ConnExpire{Conn: conn, Expire: time.Now().Add(time.Minute * 2)} // TODO: customize & randomize
						time.Sleep(time.Millisecond * 200)                                             // TODO: customize & randomize
					}
				}()
			}
		})
		for {
			connTime := <-h.preConns
			if connTime == nil {
				return errors.New("closed handler").AtWarning()
			}
			if time.Now().Before(connTime.Expire) {
				conn = connTime.Conn
				break
			}
			connTime.Conn.Close()
		}
	}

	if conn == nil {
		conn, err = dialVLESSServer(ctx, dialer, rec.Destination, command)
		if err != nil {
			return errors.New("failed to find an available destination").Base(err).AtWarning()
		}
	}
	defer conn.Close()

	ob.Conn = conn // for Vision's pre-connect

	iConn := stat.TryUnwrapStatsConn(conn)
	errors.LogInfo(ctx, "tunneling request to ", target, " via ", rec.Destination.NetAddr())

	if h.encryption != nil {
		var err error
		if conn, err = h.encryption.Handshake(conn); err != nil {
			return errors.New("ML-KEM-768 handshake failed").Base(err).AtInfo()
		}
	}

	request := &protocol.RequestHeader{
		Version: encoding.Version,
		User:    rec.User,
		Command: command,
		Address: target.Address,
		Port:    target.Port,
	}

	account := request.User.Account.(*vless.MemoryAccount)

	requestAddons := &encoding.Addons{
		Flow: account.Flow,
	}

	var input *bytes.Reader
	var rawInput *bytes.Buffer
	allowUDP443 := false
	switch requestAddons.Flow {
	case vless.XRV + "-udp443":
		allowUDP443 = true
		requestAddons.Flow = requestAddons.Flow[:16]
		fallthrough
	case vless.XRV:
		ob.CanSpliceCopy = 2
		switch request.Command {
		case protocol.RequestCommandUDP:
			if !allowUDP443 && request.Port == 443 {
				return errors.New("XTLS rejected UDP/443 traffic").AtInfo()
			}
		case protocol.RequestCommandMux:
			fallthrough // let server break Mux connections that contain TCP requests
		case protocol.RequestCommandTCP, protocol.RequestCommandRvs:
			var t reflect.Type
			var p uintptr
			if commonConn, ok := conn.(*encryption.CommonConn); ok {
				if _, ok := commonConn.Conn.(*encryption.XorConn); ok || !proxy.IsRAWTransportWithoutSecurity(iConn) {
					ob.CanSpliceCopy = 3 // full-random xorConn / non-RAW transport / another securityConn should not be penetrated
				}
				t = reflect.TypeOf(commonConn).Elem()
				p = uintptr(unsafe.Pointer(commonConn))
			} else if tlsConn, ok := iConn.(*tls.Conn); ok {
				t = reflect.TypeOf(tlsConn.Conn).Elem()
				p = uintptr(unsafe.Pointer(tlsConn.Conn))
			} else if utlsConn, ok := iConn.(*tls.UConn); ok {
				t = reflect.TypeOf(utlsConn.Conn).Elem()
				p = uintptr(unsafe.Pointer(utlsConn.Conn))
			} else if realityConn, ok := iConn.(*reality.UConn); ok {
				t = reflect.TypeOf(realityConn.Conn).Elem()
				p = uintptr(unsafe.Pointer(realityConn.Conn))
			} else {
				return errors.New("XTLS only supports TLS and REALITY directly for now.").AtWarning()
			}
			i, _ := t.FieldByName("input")
			r, _ := t.FieldByName("rawInput")
			input = (*bytes.Reader)(unsafe.Pointer(p + i.Offset))
			rawInput = (*bytes.Buffer)(unsafe.Pointer(p + r.Offset))
		default:
			panic("unknown VLESS request command")
		}
	default:
		ob.CanSpliceCopy = 3
	}

	var newCtx context.Context
	var newCancel context.CancelFunc
	if session.TimeoutOnlyFromContext(ctx) {
		newCtx, newCancel = context.WithCancel(context.Background())
	}

	sessionPolicy := h.policyManager.ForLevel(request.User.Level)
	ctx, cancel := context.WithCancel(ctx)
	timer := requestActivityTimer(ctx, request.Command, func() {
		cancel()
		if newCancel != nil {
			newCancel()
		}
	}, sessionPolicy.Timeouts.ConnectionIdle)

	clientReader := link.Reader // .(*pipe.Reader)
	clientWriter := link.Writer // .(*pipe.Writer)
	trafficState := proxy.NewTrafficState(account.ID.Bytes())
	if request.Command == protocol.RequestCommandUDP && (requestAddons.Flow == vless.XRV || (h.cone && request.Port != 53 && request.Port != 443)) {
		request.Command = protocol.RequestCommandMux
		request.Address = net.DomainAddress("v1.mux.cool")
		request.Port = net.Port(666)
	}

	postRequest := func() error {
		defer timer.SetTimeout(sessionPolicy.Timeouts.DownlinkOnly)

		bufferWriter := buf.NewBufferedWriter(buf.NewWriter(conn))
		if err := encoding.EncodeRequestHeader(bufferWriter, request, requestAddons); err != nil {
			return errors.New("failed to encode request header").Base(err).AtWarning()
		}

		// default: serverWriter := bufferWriter
		serverWriter := encoding.EncodeBodyAddons(bufferWriter, request, requestAddons, trafficState, true, ctx, conn, ob)
		if request.Command == protocol.RequestCommandMux && request.Port == 666 {
			serverWriter = xudp.NewPacketWriter(serverWriter, target, xudp.GetGlobalID(ctx))
		}
		timeoutReader, ok := clientReader.(buf.TimeoutReader)
		if ok {
			multiBuffer, err1 := timeoutReader.ReadMultiBufferTimeout(time.Millisecond * 500)
			if err1 == nil {
				if err := serverWriter.WriteMultiBuffer(multiBuffer); err != nil {
					return err // ...
				}
			} else if err1 != buf.ErrReadTimeout {
				return err1
			} else if requestAddons.Flow == vless.XRV {
				mb := make(buf.MultiBuffer, 1)
				errors.LogInfo(ctx, "Insert padding with empty content to camouflage VLESS header ", mb.Len())
				if err := serverWriter.WriteMultiBuffer(mb); err != nil {
					return err // ...
				}
			}
		} else {
			errors.LogDebug(ctx, "Reader is not timeout reader, will send out vless header separately from first payload")
		}
		// Flush; bufferWriter.WriteMultiBuffer now is bufferWriter.writer.WriteMultiBuffer
		if err := bufferWriter.SetBuffered(false); err != nil {
			return errors.New("failed to write A request payload").Base(err).AtWarning()
		}

		if requestAddons.Flow == vless.XRV {
			if tlsConn, ok := iConn.(*tls.Conn); ok {
				if tlsConn.ConnectionState().Version != gotls.VersionTLS13 {
					return errors.New(`failed to use `+requestAddons.Flow+`, found outer tls version `, tlsConn.ConnectionState().Version).AtWarning()
				}
			} else if utlsConn, ok := iConn.(*tls.UConn); ok {
				if utlsConn.ConnectionState().Version != utls.VersionTLS13 {
					return errors.New(`failed to use `+requestAddons.Flow+`, found outer tls version `, utlsConn.ConnectionState().Version).AtWarning()
				}
			}
		}
		err := buf.Copy(clientReader, serverWriter, buf.UpdateActivity(timer))
		if err != nil {
			return errors.New("failed to transfer request payload").Base(err).AtInfo()
		}

		// Indicates the end of request payload.
		switch requestAddons.Flow {
		default:
		}
		return nil
	}

	getResponse := func() error {
		defer timer.SetTimeout(sessionPolicy.Timeouts.UplinkOnly)

		responseAddons, err := encoding.DecodeResponseHeader(conn, request)
		if err != nil {
			return errors.New("failed to decode response header").Base(err).AtInfo()
		}

		// default: serverReader := buf.NewReader(conn)
		serverReader := encoding.DecodeBodyAddons(conn, request, responseAddons)
		if requestAddons.Flow == vless.XRV {
			serverReader = proxy.NewVisionReader(serverReader, trafficState, false, ctx, conn, input, rawInput, ob)
		}
		if request.Command == protocol.RequestCommandMux && request.Port == 666 {
			if requestAddons.Flow == vless.XRV {
				serverReader = xudp.NewPacketReader(&buf.BufferedReader{Reader: serverReader})
			} else {
				serverReader = xudp.NewPacketReader(conn)
			}
		}

		if requestAddons.Flow == vless.XRV {
			err = encoding.XtlsRead(serverReader, clientWriter, timer, conn, trafficState, false, ctx)
		} else {
			// from serverReader.ReadMultiBuffer to clientWriter.WriteMultiBuffer
			err = buf.Copy(serverReader, clientWriter, buf.UpdateActivity(timer))
		}

		if err != nil {
			return errors.New("failed to transfer response payload").Base(err).AtInfo()
		}

		return nil
	}

	if newCtx != nil {
		ctx = newCtx
	}

	if err := task.Run(ctx, postRequest, task.OnSuccess(getResponse, task.Close(clientWriter))); err != nil {
		return errors.New("connection ends").Base(err).AtInfo()
	}

	return nil
}

type Reverse struct {
	health                   mux.ReverseHealthConfig
	tag                      string
	dispatcher               routing.Dispatcher
	ctx                      context.Context
	cancel                   context.CancelFunc
	handler                  *Handler
	mu                       sync.Mutex
	workers                  []*reverse.BridgeWorker
	wake                     chan struct{}
	started, closed          bool
	nextAttempt, stableSince time.Time
	failures                 uint
}

func (r *Reverse) monitor() error {
	r.mu.Lock()
	defer r.mu.Unlock()
	if r.closed {
		return nil
	}
	now := time.Now()
	kept := r.workers[:0]
	ready, pending, busy := 0, 0, 0
	for _, w := range r.workers {
		if w.Closed() {
			continue
		}
		kept = append(kept, w)
		state := w.Worker.ReverseHealth().Snapshot().State
		if state == "READY" {
			ready++
			if w.Connections() > 0 {
				busy++
			}
		} else if state == "VALIDATING" {
			pending++
		}
	}
	r.workers = kept
	idle := ready - busy
	for i := len(r.workers) - 1; i >= 0 && idle > r.health.MaxIdleReadyWorkers; i-- {
		w := r.workers[i]
		if w.Connections() == 0 && w.Worker.ReverseHealth().Snapshot().State == "READY" {
			w.Worker.ReverseHealth().Drain()
			idle--
			ready--
		}
	}
	r.reportDialDemand(ready, pending, !now.Before(r.nextAttempt))
	if ready >= r.health.MinHealthyWorkers {
		if r.stableSince.IsZero() {
			r.stableSince = now
		}
		if now.Sub(r.stableSince) >= r.health.StableReset {
			r.failures = 0
		}
	} else {
		r.stableSince = time.Time{}
	}
	// Keep the configured spare capacity without exceeding the healthy pool limit.
	desired := r.desiredWorkers(busy)
	if ready+pending >= desired || pending >= r.health.MaxParallelDials || now.Before(r.nextAttempt) {
		return nil
	}
	for ready+pending < desired && pending < r.health.MaxParallelDials {
		if !r.acquireDial() {
			return nil
		}
		reader1, writer1 := pipe.New(pipe.WithSizeLimit(2 * buf.Size))
		reader2, writer2 := pipe.New(pipe.WithSizeLimit(2 * buf.Size))
		link1 := &transport.Link{Reader: reader1, Writer: writer2}
		link2 := &transport.Link{Reader: reader2, Writer: writer1}
		w := &reverse.BridgeWorker{Tag: r.tag, Dispatcher: r.dispatcher}
		config := r.health
		config.Wake = r.wake
		worker, err := mux.NewReverseServerWorker(session.ContextWithIsReverseMux(r.ctx, true), w, link1, config)
		if err != nil {
			releaseReverseDial()
			common.Interrupt(reader1)
			common.Interrupt(reader2)
			return err
		}
		w.Worker = worker
		r.workers = append(r.workers, w)
		pending++
		go func() {
			defer releaseReverseDial()
			timer := time.NewTicker(25 * time.Millisecond)
			defer timer.Stop()
			for {
				state := worker.ReverseHealth().Snapshot().State
				if state != "VALIDATING" {
					return
				}
				select {
				case <-worker.WaitClosed():
					return
				case <-timer.C:
				}
			}
		}()
		ctx, cancel := context.WithCancel(session.ContextWithOutbounds(r.ctx, []*session.Outbound{{Target: net.Destination{Address: net.DomainAddress("v1.rvs.cool")}}}))
		go func() {
			select {
			case <-worker.WaitClosed():
				cancel()
			case <-ctx.Done():
				worker.Close()
			}
		}()
		go func() {
			defer cancel()
			defer worker.Close()
			defer common.Interrupt(reader1)
			defer common.Interrupt(reader2)
			if err := r.handler.Process(ctx, link2, session.FullHandlerFromContext(ctx).(*proxyman.Handler)); err != nil {
				errors.LogInfoInner(ctx, err, "reverse transport ended")
			}
		}()
	}
	base := r.backoffBase()
	r.failures++
	r.nextAttempt = now.Add(base/2 + time.Duration(now.UnixNano()%int64(base/2)))
	return nil
}
func (r *Reverse) Start() error {
	r.mu.Lock()
	if r.closed || r.started {
		r.mu.Unlock()
		return nil
	}
	r.started = true
	r.mu.Unlock()
	// Handler construction precedes core Start. Cancellation must win over delayed initialization.
	timer := time.NewTimer(2 * time.Second)
	defer timer.Stop()
	select {
	case <-r.ctx.Done():
		return nil
	case <-timer.C:
	}
	ticker := time.NewTicker(100 * time.Millisecond)
	defer ticker.Stop()
	for {
		r.monitor()
		select {
		case <-r.ctx.Done():
			return nil
		case <-r.wake:
		case <-ticker.C:
		}
	}
}
func (r *Reverse) Close() error {
	r.mu.Lock()
	if r.closed {
		r.mu.Unlock()
		return nil
	}
	r.closed = true
	workers := r.workers
	r.workers = nil
	r.mu.Unlock()
	r.forgetDialDemand()
	if r.cancel != nil {
		r.cancel()
	}
	for _, w := range workers {
		w.Worker.Close()
	}
	return nil
}

func (r *Reverse) desiredWorkers(busy int) int {
	return min(max(r.health.MinHealthyWorkers, busy+int(r.health.SpareWorkers)), int(r.health.MaxHealthyWorkers))
}
func (r *Reverse) backoffBase() time.Duration {
	shift := min(r.failures, 10)
	if r.health.BackoffBase > r.health.BackoffCap>>shift {
		return r.health.BackoffCap
	}
	return min(r.health.BackoffBase<<shift, r.health.BackoffCap)
}
