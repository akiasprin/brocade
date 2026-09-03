package conf_test

import (
	"encoding/json"
	"testing"

	"github.com/xtls/xray-core/common/net"
	"github.com/xtls/xray-core/common/protocol"
	"github.com/xtls/xray-core/common/serial"
	. "github.com/xtls/xray-core/infra/conf"
	proxyanytls "github.com/xtls/xray-core/proxy/anytls"
	"google.golang.org/protobuf/proto"
)

func TestAnyTLSServerConfigBuild(t *testing.T) {
	var config AnyTLSServerConfig
	err := json.Unmarshal([]byte(`{
		"users": [
			{"password": "alice-password", "email": "alice@example.com", "level": 2},
			{"password": "bob-password"}
		],
		"paddingScheme": ["stop=2", "0=30-30"]
	}`), &config)
	if err != nil {
		t.Fatal(err)
	}
	actual, err := config.Build()
	if err != nil {
		t.Fatal(err)
	}
	expected := &proxyanytls.ServerConfig{
		Users: []*protocol.User{
			{
				Email: "alice@example.com", Level: 2,
				Account: serial.ToTypedMessage(&proxyanytls.Account{Password: "alice-password"}),
			},
			{
				Account: serial.ToTypedMessage(&proxyanytls.Account{Password: "bob-password"}),
			},
		},
		PaddingScheme: "stop=2\n0=30-30",
	}
	if !proto.Equal(actual, expected) {
		t.Fatalf("server config = %v, want %v", actual, expected)
	}
}

func TestAnyTLSClientConfigBuild(t *testing.T) {
	var config AnyTLSClientConfig
	err := json.Unmarshal([]byte(`{
		"address": "anytls.example.com",
		"port": 443,
		"email": "client@example.com",
		"password": "client-password",
		"level": 3,
		"idleSessionCheckInterval": 11,
		"idleSessionTimeout": 22,
		"minIdleSession": 2
	}`), &config)
	if err != nil {
		t.Fatal(err)
	}
	actual, err := config.Build()
	if err != nil {
		t.Fatal(err)
	}
	expected := &proxyanytls.ClientConfig{
		Server: &protocol.ServerEndpoint{
			Address: net.NewIPOrDomain(net.DomainAddress("anytls.example.com")),
			Port:    443,
			User: &protocol.User{
				Email: "client@example.com", Level: 3,
				Account: serial.ToTypedMessage(&proxyanytls.Account{Password: "client-password"}),
			},
		},
		IdleSessionCheckInterval: 11,
		IdleSessionTimeout:       22,
		MinIdleSession:           2,
	}
	if !proto.Equal(actual, expected) {
		t.Fatalf("client config = %v, want %v", actual, expected)
	}
}

func TestAnyTLSMasqueradeConfigBuild(t *testing.T) {
	var config AnyTLSServerConfig
	if err := json.Unmarshal([]byte(`{
		"users": [{"password": "server-password"}],
		"masquerade": {
			"type": "string",
			"content": "Forbidden",
			"statusCode": 403,
			"headers": {"Content-Type": "text/plain"}
		}
	}`), &config); err != nil {
		t.Fatal(err)
	}

	actual, err := config.Build()
	if err != nil {
		t.Fatal(err)
	}
	expected := &proxyanytls.ServerConfig{
		Users: []*protocol.User{{
			Account: serial.ToTypedMessage(&proxyanytls.Account{Password: "server-password"}),
		}},
		Masquerade: &proxyanytls.Masquerade{
			Type:       "string",
			Content:    "Forbidden",
			StatusCode: 403,
			Headers:    map[string]string{"Content-Type": "text/plain"},
		},
	}
	if !proto.Equal(actual, expected) {
		t.Fatalf("server config = %v, want %v", actual, expected)
	}
}

func TestAnyTLSConfigRejectsInvalidValues(t *testing.T) {
	serverTests := []AnyTLSServerConfig{
		{Users: []*AnyTLSUser{{Password: ""}}},
		{Users: []*AnyTLSUser{nil}},
	}
	for _, config := range serverTests {
		if _, err := config.Build(); err == nil {
			t.Fatalf("server config %+v unexpectedly succeeded", config)
		}
	}

	clientTests := []AnyTLSClientConfig{
		{Password: "password"},
		{Address: &Address{Address: net.DomainAddress("example.com")}},
		{Address: &Address{Address: net.DomainAddress("example.com")}, Password: "password", MinIdleSession: -1},
	}
	for _, config := range clientTests {
		if _, err := config.Build(); err == nil {
			t.Fatalf("client config %+v unexpectedly succeeded", config)
		}
	}
}
