package dns

import (
	"context"
	"math/rand"
	"strings"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
	"github.com/miekg/dns"
	"github.com/xtls/xray-core/common"
	"github.com/xtls/xray-core/common/net"
	dns_feature "github.com/xtls/xray-core/features/dns"
	"golang.org/x/net/dns/dnsmessage"
)

func TestLongDomainReportsBothQueryErrors(t *testing.T) {
	cache := &CacheController{name: "invalid-domain-test"}
	for name, send := range map[string]func(context.Context, chan<- error, string, dns_feature.IPOption){
		"udp":  (&ClassicNameServer{cacheController: cache}).sendQuery,
		"tcp":  (&TCPNameServer{cacheController: cache}).sendQuery,
		"doh":  (&DoHNameServer{cacheController: cache}).sendQuery,
		"quic": (&QUICNameServer{cacheController: cache}).sendQuery,
	} {
		t.Run(name, func(t *testing.T) {
			results := make(chan error, 2)
			send(context.Background(), results, strings.Repeat("a", 256), dns_feature.IPOption{IPv4Enable: true, IPv6Enable: true})
			for range 2 {
				select {
				case err := <-results:
					if err == nil {
						t.Fatal("accepted an oversized DNS name")
					}
				default:
					t.Fatal("did not notify both A and AAAA waiters")
				}
			}
		})
	}
}

func Test_parseResponse(t *testing.T) {
	var p [][]byte

	ans := new(dns.Msg)
	ans.Id = 0
	p = append(p, common.Must2(ans.Pack()))

	p = append(p, []byte{})

	ans = new(dns.Msg)
	ans.Id = 1
	ans.Answer = append(
		ans.Answer,
		common.Must2(dns.NewRR("google.com. IN CNAME m.test.google.com")),
		common.Must2(dns.NewRR("google.com. IN CNAME fake.google.com")),
		common.Must2(dns.NewRR("google.com. IN A 8.8.8.8")),
		common.Must2(dns.NewRR("google.com. IN A 8.8.4.4")),
	)
	p = append(p, common.Must2(ans.Pack()))

	ans = new(dns.Msg)
	ans.Id = 2
	ans.Answer = append(
		ans.Answer,
		common.Must2(dns.NewRR("google.com. IN CNAME m.test.google.com")),
		common.Must2(dns.NewRR("google.com. IN CNAME fake.google.com")),
		common.Must2(dns.NewRR("google.com. IN CNAME m.test.google.com")),
		common.Must2(dns.NewRR("google.com. IN CNAME test.google.com")),
		common.Must2(dns.NewRR("google.com. IN AAAA 2001:4860:4860::8888")),
		common.Must2(dns.NewRR("google.com. IN AAAA 2001:4860:4860::8844")),
	)
	p = append(p, common.Must2(ans.Pack()))

	tests := []struct {
		name    string
		want    *IPRecord
		wantErr bool
	}{
		{
			"empty",
			&IPRecord{RCode: dnsmessage.RCodeSuccess},
			false,
		},
		{
			"error",
			nil,
			true,
		},
		{
			"a record",
			&IPRecord{
				ReqID: 1,
				IP:    []net.IP{net.ParseIP("8.8.8.8"), net.ParseIP("8.8.4.4")},
				RCode: dnsmessage.RCodeSuccess,
			},
			false,
		},
		{
			"aaaa record",
			&IPRecord{ReqID: 2, IP: []net.IP{net.ParseIP("2001:4860:4860::8888"), net.ParseIP("2001:4860:4860::8844")}, RCode: dnsmessage.RCodeSuccess},
			false,
		},
	}
	for i, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got, err := parseResponse(p[i])
			if (err != nil) != tt.wantErr {
				t.Errorf("handleResponse() error = %v, wantErr %v", err, tt.wantErr)
				return
			}

			if got != nil {
				// reset the time and RawHeader
				got.Expire = time.Time{}
				got.RawHeader = nil
			}
			if cmp.Diff(got, tt.want) != "" {
				t.Error(cmp.Diff(got, tt.want))
				// t.Errorf("handleResponse() = %#v, want %#v", got, tt.want)
			}
		})
	}
}

func Test_buildReqMsgs(t *testing.T) {
	stubID := func() uint16 {
		return uint16(rand.Uint32())
	}
	type args struct {
		domain  string
		option  dns_feature.IPOption
		reqOpts *dnsmessage.Resource
	}
	tests := []struct {
		name string
		args args
		want int
	}{
		{"dual stack", args{"test.com", dns_feature.IPOption{
			IPv4Enable: true,
			IPv6Enable: true,
			FakeEnable: false,
		}, nil}, 2},
		{"ipv4 only", args{"test.com", dns_feature.IPOption{
			IPv4Enable: true,
			IPv6Enable: false,
			FakeEnable: false,
		}, nil}, 1},
		{"ipv6 only", args{"test.com", dns_feature.IPOption{
			IPv4Enable: false,
			IPv6Enable: true,
			FakeEnable: false,
		}, nil}, 1},
		{"none/error", args{"test.com", dns_feature.IPOption{
			IPv4Enable: false,
			IPv6Enable: false,
			FakeEnable: false,
		}, nil}, 0},
		{"name too long", args{strings.Repeat("a", 256), dns_feature.IPOption{
			IPv4Enable: true,
			IPv6Enable: true,
			FakeEnable: false,
		}, nil}, 0},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got, err := buildReqMsgs(tt.args.domain, tt.args.option, stubID, tt.args.reqOpts)
			if (err != nil) != (tt.name == "name too long") {
				t.Fatalf("buildReqMsgs() error = %v", err)
			}
			if len(got) != tt.want {
				t.Errorf("buildReqMsgs() = %v, want %v", got, tt.want)
			}
		})
	}
}

func Test_genEDNS0Options(t *testing.T) {
	type args struct {
		clientIP net.IP
	}
	tests := []struct {
		name string
		args args
		want *dnsmessage.Resource
	}{
		// TODO: Add test cases.
		{"ipv4", args{net.ParseIP("4.3.2.1")}, nil},
		{"ipv6", args{net.ParseIP("2001::4321")}, nil},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := genEDNS0Options(tt.args.clientIP, 0); got == nil {
				t.Errorf("genEDNS0Options() = %v, want %v", got, tt.want)
			}
		})
	}
}

func TestFqdn(t *testing.T) {
	type args struct {
		domain string
	}
	tests := []struct {
		name string
		args args
		want string
	}{
		{"with fqdn", args{"www.example.com."}, "www.example.com."},
		{"without fqdn", args{"www.example.com"}, "www.example.com."},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := Fqdn(tt.args.domain); got != tt.want {
				t.Errorf("Fqdn() = %v, want %v", got, tt.want)
			}
		})
	}
}
