package dns

import (
	"context"
	"sync"
	"testing"

	"github.com/xtls/xray-core/common/session"
)

func TestDNSDispatchContentIsIsolatedFromParentAndOtherResolvers(t *testing.T) {
	parent := &session.Content{
		Protocol:       "tls",
		SkipDNSResolve: true,
		Attributes: map[string]string{
			"forcedOutboundTag": "dns-egress",
			"original":          "preserved",
		},
	}
	inbound := &session.Inbound{Tag: "client"}
	ctx := session.ContextWithInbound(session.ContextWithContent(context.Background(), parent), inbound)
	const resolvers = 16
	var workers sync.WaitGroup
	for range resolvers {
		workers.Add(1)
		go func() {
			defer workers.Done()
			dnsCtx := toDnsContext(ctx, "resolver")
			content := session.ContentFromContext(dnsCtx)
			if content == parent || content.Protocol != "tls" || !content.SkipDNSResolve {
				t.Error("DNS stream must own its content and retain routing settings")
				return
			}
			if session.InboundFromContext(dnsCtx) != inbound || content.Attribute("forcedOutboundTag") != "dns-egress" {
				t.Error("DNS stream lost its inbound or outbound routing scope")
			}
			for range 1000 {
				content.SetAttribute("original", "changed")
				content.SetAttribute("xray.sniffing", "failed")
				_ = content.Attribute("original")
			}
			content.Protocol = "dns"
		}()
	}
	workers.Wait()
	if parent.Protocol != "tls" || parent.Attribute("original") != "preserved" || parent.Attribute("xray.sniffing") != "" {
		t.Fatal("DNS dispatch changed parent connection content")
	}
	if len(parent.Attributes) != 2 {
		t.Fatal("DNS dispatch leaked routing attributes into its parent")
	}
}
