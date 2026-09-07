package inbound

import (
	"context"
	"net/http"
	"time"

	"github.com/xtls/xray-core/common/mux"
	"github.com/xtls/xray-core/common/net"
	"github.com/xtls/xray-core/transport/internet/tagged"
)

// The canary uses the same tagged outbound, picker and TCP dispatch as business
// traffic. A backend failure changes only this verdict, never worker ACK health.
func (r *Reverse) runCanary(ctx context.Context) {
	observation := mux.NewReverseCanary(r.tag)
	defer observation.Close()
	tr := &http.Transport{DisableKeepAlives: true, DialContext: func(ctx context.Context, network, address string) (net.Conn, error) {
		dest, err := net.ParseDestination(network + ":" + address)
		if err != nil {
			return nil, err
		}
		return tagged.Dialer(ctx, r.dispatcher, dest, r.tag)
	}}
	defer tr.CloseIdleConnections()
	client := &http.Client{Transport: tr, Timeout: 750 * time.Millisecond, CheckRedirect: func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse }}
	ticker := time.NewTicker(time.Second)
	defer ticker.Stop()
	for {
		started := time.Now()
		reason := "success"
		request, err := http.NewRequestWithContext(ctx, http.MethodGet, r.canaryURL, nil)
		if err != nil {
			return
		}
		response, err := client.Do(request)
		if err != nil {
			reason = "request_failed"
		} else {
			if response.StatusCode < 200 || response.StatusCode >= 400 {
				reason = "http_status_failed"
			}
			response.Body.Close()
		}
		if ctx.Err() != nil {
			return
		}
		observation.Record(started, reason)
		select {
		case <-ctx.Done():
			return
		case <-ticker.C:
		}
	}
}
