package conf_test

import (
	"encoding/json"
	"testing"

	"github.com/xtls/xray-core/infra/conf"
	"github.com/xtls/xray-core/transport/internet/finalmask/udphop"
)

func TestLegacyQUICUDPHopConfiguration(t *testing.T) {
	for _, test := range []struct {
		name     string
		interval string
		min, max int64
	}{
		{"default", "", 30, 30},
		{"range", `,"interval":"5-10"`, 5, 10},
	} {
		t.Run(test.name, func(t *testing.T) {
			var config conf.StreamConfig
			input := `{"network":"hysteria","finalmask":{"quicParams":{"udpHop":{"ports":"443,8443-8444"` + test.interval + `}}}}`
			if err := json.Unmarshal([]byte(input), &config); err != nil {
				t.Fatal(err)
			}
			built, err := config.Build()
			if err != nil {
				t.Fatal(err)
			}
			if len(built.Udpmasks) != 1 {
				t.Fatalf("expected one compatibility mask, got %d", len(built.Udpmasks))
			}
			mask, err := built.Udpmasks[0].GetInstance()
			if err != nil {
				t.Fatal(err)
			}
			hop := mask.(*udphop.Config)
			if !hop.LegacyQuic || hop.IntervalMin != test.min || hop.IntervalMax != test.max {
				t.Fatalf("legacy mode or interval lost: %v", hop)
			}
			if len(hop.RemotePorts) != 3 || hop.RemotePorts[0] != 443 || hop.RemotePorts[1] != 8443 || hop.RemotePorts[2] != 8444 {
				t.Fatalf("legacy ports lost: %v", hop.RemotePorts)
			}
		})
	}
	var invalid conf.StreamConfig
	if err := json.Unmarshal([]byte(`{"finalmask":{"quicParams":{"udpHop":{"ports":"443","interval":4}}}}`), &invalid); err != nil {
		t.Fatal(err)
	}
	if _, err := invalid.Build(); err == nil {
		t.Fatal("accepted legacy hopping faster than the supported minimum")
	}
}
