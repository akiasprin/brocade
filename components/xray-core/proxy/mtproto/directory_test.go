package mtproto

import "testing"

func TestParseOfficialProxyConfig(t *testing.T) {
	config := `
# force_probability 10 10
default 2;
proxy_for 1 149.154.175.50:8888;
proxy_for -1 149.154.175.50:8888;
proxy_for 2 149.154.161.144:8888;
proxy_for -4 149.154.164.250:8888;
`
	defaultDC, targets, err := parseProxyConfig(config)
	if err != nil {
		t.Fatal(err)
	}
	if defaultDC != 2 {
		t.Fatalf("default dc = %d", defaultDC)
	}
	if got := targets[-4]; len(got) != 1 || got[0] != (proxyTarget{host: "149.154.164.250", port: 8888}) {
		t.Fatalf("dc -4 targets = %#v", got)
	}
}
