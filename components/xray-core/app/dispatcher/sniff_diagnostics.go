package dispatcher

import (
	"context"
	stderrors "errors"
	"fmt"
	"io"
	"time"

	"github.com/xtls/xray-core/common"
	"github.com/xtls/xray-core/common/buf"
	"github.com/xtls/xray-core/common/errors"
	"github.com/xtls/xray-core/common/net"
	"github.com/xtls/xray-core/common/protocol"
	"github.com/xtls/xray-core/common/session"
)

// Store values, not mutable session pointers. Formatting is deferred until the
// Debug message is rendered; domains are quoted so input cannot forge log lines.
type sniffDecision struct {
	original                                net.Destination
	source, result, reason, domain, applied string
	changed, metadataOnly                   bool
	elapsed                                 time.Duration
}

func (s sniffDecision) String() string {
	return fmt.Sprintf("sniff original=%q source=%q result=%s reason=%s domain=%q applied=%s changed=%t metadata_only=%t elapsed_ms=%.3f",
		s.original.String(), s.source, s.result, s.reason, s.domain, s.applied, s.changed, s.metadataOnly, float64(s.elapsed)/float64(time.Millisecond))
}

func sniffFailureReason(err error) string {
	switch {
	case stderrors.Is(err, errSniffingTimeout), stderrors.Is(err, buf.ErrReadTimeout), stderrors.Is(err, context.DeadlineExceeded):
		return "timeout"
	case stderrors.Is(err, errSniffingAttemptLimit):
		return "attempt_limit"
	case stderrors.Is(err, context.Canceled):
		return "canceled"
	case stderrors.Is(err, io.EOF):
		return "eof"
	case stderrors.Is(err, protocol.ErrProtoNeedMoreData), stderrors.Is(err, io.ErrUnexpectedEOF):
		return "need_more_data"
	case stderrors.Is(err, errUnknownContent), stderrors.Is(err, common.ErrNoClue):
		return "unrecognized"
	default:
		return "read_or_parser_error"
	}
}

func makeSniffDecision(request session.SniffingRequest, original net.Destination, result SniffResult, err error, reason, applied string, elapsed time.Duration) sniffDecision {
	d := sniffDecision{original: original, source: "none", result: "disabled", reason: reason, applied: applied, metadataOnly: request.MetadataOnly, elapsed: elapsed}
	if !request.Enabled {
		return d
	}
	if err != nil {
		d.result, d.reason = "failed", sniffFailureReason(err)
		return d
	}
	if result == nil {
		d.result, d.reason = "failed", "no_result"
		return d
	}
	d.source, d.domain, d.result = result.Protocol(), result.Domain(), "success"
	if composite, ok := result.(SnifferResultComposite); ok {
		d.source = composite.ProtocolForDomainResult()
	}
	if d.domain == "" {
		d.result = "protocol_only"
	}
	d.changed = applied != "none" && d.domain != "" && original.Address.String() != net.ParseAddress(d.domain).String()
	return d
}

func logSniffDecision(ctx context.Context, request session.SniffingRequest, original net.Destination, result SniffResult, err error, reason, applied string, elapsed time.Duration) {
	decision := makeSniffDecision(request, original, result, err, reason, applied, elapsed)
	// Generated Brocade configs normally run at warning level. Surface exactly the case that
	// setSniffingState exposes to the xray.sniffing=failed routing fallback, while keeping ordinary
	// successful decisions at Debug so every HTTP, TLS, and QUIC connection does not flood the log.
	if request.Enabled && original.Address.Family().IsIP() && (err != nil || reason != "accepted") {
		errors.LogWarning(ctx, decision)
		return
	}
	errors.LogDebug(ctx, decision)
}
