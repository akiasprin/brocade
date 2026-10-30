package dns

import (
	"bytes"
	"context"
	"encoding/binary"
	"net/url"
	"sync/atomic"
	"time"

	"github.com/xtls/xray-core/common/buf"
	"github.com/xtls/xray-core/common/errors"
	"github.com/xtls/xray-core/common/net"
	"github.com/xtls/xray-core/common/net/cnc"
	"github.com/xtls/xray-core/common/protocol/dns"
	"github.com/xtls/xray-core/common/session"
	dns_feature "github.com/xtls/xray-core/features/dns"
	"github.com/xtls/xray-core/features/routing"
	"github.com/xtls/xray-core/transport/internet"
)

// TCPNameServer implemented DNS over TCP (RFC7766).
type TCPNameServer struct {
	cacheController *CacheController
	destination     *net.Destination
	reqID           uint32
	dial            func(context.Context) (net.Conn, error)
	clientIP        net.IP
}

// NewTCPNameServer creates DNS over TCP server object for remote resolving.
func NewTCPNameServer(
	url *url.URL,
	dispatcher routing.Dispatcher,
	disableCache bool, serveStale bool, serveExpiredTTL uint32,
	clientIP net.IP,
) (*TCPNameServer, error) {
	s, err := baseTCPNameServer(url, "TCP", disableCache, serveStale, serveExpiredTTL, clientIP)
	if err != nil {
		return nil, err
	}

	s.dial = func(ctx context.Context) (net.Conn, error) {
		dnsCtx := queryNetworkContext{Context: toDnsContext(ctx, s.destination.String()), request: ctx}
		link, err := dispatcher.Dispatch(dnsCtx, *s.destination)
		if err != nil {
			return nil, err
		}

		return cnc.NewConnection(
			cnc.ConnectionInputMulti(link.Writer),
			cnc.ConnectionOutputMulti(link.Reader),
		), nil
	}

	errors.LogInfo(context.Background(), "DNS: created TCP client initialized for ", url.String())
	return s, nil
}

// NewTCPLocalNameServer creates DNS over TCP client object for local resolving
func NewTCPLocalNameServer(url *url.URL, disableCache bool, serveStale bool, serveExpiredTTL uint32, clientIP net.IP) (*TCPNameServer, error) {
	s, err := baseTCPNameServer(url, "TCPL", disableCache, serveStale, serveExpiredTTL, clientIP)
	if err != nil {
		return nil, err
	}

	s.dial = func(ctx context.Context) (net.Conn, error) {
		return internet.DialSystem(ctx, *s.destination, nil)
	}

	errors.LogInfo(context.Background(), "DNS: created Local TCP client initialized for ", url.String())
	return s, nil
}

func baseTCPNameServer(url *url.URL, prefix string, disableCache bool, serveStale bool, serveExpiredTTL uint32, clientIP net.IP) (*TCPNameServer, error) {
	port := net.Port(53)
	if url.Port() != "" {
		var err error
		if port, err = net.PortFromString(url.Port()); err != nil {
			return nil, err
		}
	}
	dest := net.TCPDestination(net.ParseAddress(url.Hostname()), port)

	s := &TCPNameServer{
		cacheController: NewCacheController(prefix+"//"+dest.NetAddr(), disableCache, serveStale, serveExpiredTTL),
		destination:     &dest,
		clientIP:        clientIP,
	}

	return s, nil
}

// Name implements Server.
func (s *TCPNameServer) Name() string {
	return s.cacheController.name
}

// IsDisableCache implements Server.
func (s *TCPNameServer) IsDisableCache() bool {
	return s.cacheController.disableCache
}

func (s *TCPNameServer) newReqID() uint16 {
	return uint16(atomic.AddUint32(&s.reqID, 1))
}

// getCacheController implements CachedNameserver.
func (s *TCPNameServer) getCacheController() *CacheController {
	return s.cacheController
}

// sendQuery implements CachedNameserver.
func (s *TCPNameServer) sendQuery(ctx context.Context, noResponseErrCh chan<- error, fqdn string, option dns_feature.IPOption) {
	errors.LogInfo(ctx, s.Name(), " querying DNS for: ", fqdn)

	reqs, err := buildReqMsgs(fqdn, option, s.newReqID, genEDNS0Options(s.clientIP, 0))
	if err != nil {
		errors.LogErrorInner(ctx, err, "failed to build dns query for ", fqdn)
		if noResponseErrCh != nil {
			if option.IPv4Enable {
				reportQueryError(ctx, noResponseErrCh, err)
			}
			if option.IPv6Enable {
				reportQueryError(ctx, noResponseErrCh, err)
			}
		}
		return
	}

	var deadline time.Time
	if d, ok := ctx.Deadline(); ok {
		deadline = d
	} else {
		deadline = time.Now().Add(time.Second * 5)
	}

	for _, req := range reqs {
		if !reserveQuery(ctx) {
			break
		}
		go func(r *dnsRequest) {
			dnsCtx := ctx

			if inbound := session.InboundFromContext(ctx); inbound != nil {
				dnsCtx = session.ContextWithInbound(dnsCtx, inbound)
			}

			dnsCtx = session.ContextWithContent(dnsCtx, &session.Content{
				Protocol:       "dns",
				SkipDNSResolve: true,
			})

			var cancel context.CancelFunc
			dnsCtx, cancel = context.WithDeadline(dnsCtx, deadline)
			defer cancel()

			b, err := dns.PackMessage(r.msg)
			if err != nil {
				errors.LogErrorInner(ctx, err, "failed to pack dns query")
				if noResponseErrCh != nil {
					reportQueryError(ctx, noResponseErrCh, err)
				}
				return
			}

			conn, err := s.dial(dnsCtx)
			if err != nil {
				errors.LogErrorInner(ctx, err, "failed to dial namesever")
				if noResponseErrCh != nil {
					reportQueryError(ctx, noResponseErrCh, err)
				}
				return
			}
			defer conn.Close()
			// Some dispatched connections do not implement deadlines. Cancellation
			// still has to unblock reads on both dispatched and local TCP sockets.
			stopClose := context.AfterFunc(dnsCtx, func() { conn.Close() })
			defer stopClose()
			_ = conn.SetDeadline(deadline)
			dnsReqBuf := buf.New()
			err = binary.Write(dnsReqBuf, binary.BigEndian, uint16(b.Len()))
			if err != nil {
				errors.LogErrorInner(ctx, err, "binary write failed")
				if noResponseErrCh != nil {
					reportQueryError(ctx, noResponseErrCh, err)
				}
				return
			}
			_, err = dnsReqBuf.Write(b.Bytes())
			if err != nil {
				errors.LogErrorInner(ctx, err, "buffer write failed")
				if noResponseErrCh != nil {
					reportQueryError(ctx, noResponseErrCh, err)
				}
				return
			}
			b.Release()

			_, err = conn.Write(dnsReqBuf.Bytes())
			if err != nil {
				errors.LogErrorInner(ctx, err, "failed to send query")
				if noResponseErrCh != nil {
					reportQueryError(ctx, noResponseErrCh, err)
				}
				return
			}
			dnsReqBuf.Release()

			respBuf := buf.New()
			defer respBuf.Release()
			n, err := respBuf.ReadFullFrom(conn, 2)
			if err != nil && n == 0 {
				errors.LogErrorInner(ctx, err, "failed to read response length")
				if noResponseErrCh != nil {
					reportQueryError(ctx, noResponseErrCh, err)
				}
				return
			}
			var length uint16
			err = binary.Read(bytes.NewReader(respBuf.Bytes()), binary.BigEndian, &length)
			if err != nil {
				errors.LogErrorInner(ctx, err, "failed to parse response length")
				if noResponseErrCh != nil {
					reportQueryError(ctx, noResponseErrCh, err)
				}
				return
			}
			respBuf.Clear()
			n, err = respBuf.ReadFullFrom(conn, int32(length))
			if err != nil && n == 0 {
				errors.LogErrorInner(ctx, err, "failed to read response length")
				if noResponseErrCh != nil {
					reportQueryError(ctx, noResponseErrCh, err)
				}
				return
			}

			rec, err := parseResponse(respBuf.Bytes())
			if err != nil {
				errors.LogErrorInner(ctx, err, "failed to parse DNS over TCP response")
				if noResponseErrCh != nil {
					reportQueryError(ctx, noResponseErrCh, err)
				}
				return
			}

			if dnsCtx.Err() == nil {
				s.cacheController.updateRecord(r, rec)
			}
		}(req)
	}
}

// QueryIP implements Server.
func (s *TCPNameServer) QueryIP(ctx context.Context, domain string, option dns_feature.IPOption) ([]net.IP, uint32, error) {
	return queryIP(ctx, s, domain, option)
}
