package conf_test

import (
	"encoding/json"
	"testing"

	. "github.com/xtls/xray-core/infra/conf"
)

func TestTLSCertificateReloadInterval(t *testing.T) {
	var config TLSCertConfig
	if err := json.Unmarshal([]byte(`{"reloadInterval":5}`), &config); err != nil {
		t.Fatal(err)
	}
	if config.ReloadInterval != 5 {
		t.Fatalf("reloadInterval = %d, want 5", config.ReloadInterval)
	}
}
