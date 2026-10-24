package dns

import (
	"bytes"
	"testing"

	"github.com/xtls/xray-core/common/buf"
	"github.com/xtls/xray-core/common/net"
	"github.com/xtls/xray-core/common/signal"
	feature_dns "github.com/xtls/xray-core/features/dns"
	"golang.org/x/net/dns/dnsmessage"
)

type sharedResultClient struct{ ips []net.IP }

func (*sharedResultClient) Type() interface{} { return feature_dns.ClientType() }
func (*sharedResultClient) Start() error      { return nil }
func (*sharedResultClient) Close() error      { return nil }
func (c *sharedResultClient) LookupIP(string, feature_dns.IPOption) ([]net.IP, uint32, error) {
	return c.ips, 60, nil
}

type capturedMessage struct{ data []byte }

func (w *capturedMessage) WriteMessage(message *buf.Buffer) error {
	defer message.Release()
	w.data = bytes.Clone(message.Bytes())
	return nil
}

func TestIPQueryPreservesSharedClientResults(t *testing.T) {
	for _, tc := range []struct {
		name  string
		qType dnsmessage.Type
		ip    net.IP
	}{
		{"A", dnsmessage.TypeA, net.ParseIP("192.0.2.1")},
		{"AAAA", dnsmessage.TypeAAAA, net.ParseIP("2001:db8::1")},
	} {
		t.Run(tc.name, func(t *testing.T) {
			client := &sharedResultClient{ips: []net.IP{tc.ip}}
			original := bytes.Clone(client.ips[0])
			handler := &Handler{client: client}
			writer := &capturedMessage{}
			handler.handleIPQuery(42, tc.qType, "example.test.", writer, signal.NewNoopActivityTimer())
			if !bytes.Equal(client.ips[0], original) {
				t.Fatal("DNS response normalization changed the client's cached result")
			}
			var response dnsmessage.Message
			if err := response.Unpack(writer.data); err != nil {
				t.Fatal(err)
			}
			if response.ID != 42 || len(response.Answers) != 1 || response.Answers[0].Header.Type != tc.qType {
				t.Fatalf("unexpected DNS response: %+v", response)
			}
		})
	}
}
