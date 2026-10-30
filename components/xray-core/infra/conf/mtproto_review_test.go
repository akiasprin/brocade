package conf_test

import (
	"testing"

	"github.com/xtls/xray-core/infra/conf"
	"github.com/xtls/xray-core/proxy/mtproto"
)

func TestMTProtoServerConfigPreservesIdentityAndSecret(t *testing.T) {
	input := &conf.MTProtoServerConfig{Users: []*conf.MTProtoUser{{Secret: "00010203-0405-0607-0809-0a0b0c0d0e0f", Email: "alice@tenant#entry", Level: 7}}}
	message, err := input.Build()
	if err != nil {
		t.Fatal(err)
	}
	server, ok := message.(*mtproto.ServerConfig)
	if !ok || len(server.Users) != 1 {
		t.Fatalf("unexpected config %T", message)
	}
	user := server.Users[0]
	if user.Email != "alice@tenant#entry" || user.Level != 7 {
		t.Fatal("user identity or policy level changed")
	}
	account, err := user.Account.GetInstance()
	if err != nil {
		t.Fatal(err)
	}
	if account.(*mtproto.Account).Secret != input.Users[0].Secret {
		t.Fatal("configured credential changed during build")
	}
}
func TestMTProtoServerConfigRejectsIncompleteUsers(t *testing.T) {
	for name, user := range map[string]*conf.MTProtoUser{"nil": nil, "missing-email": {Secret: "00010203-0405-0607-0809-0a0b0c0d0e0f"}, "missing-secret": {Email: "alice@tenant#entry"}, "short-secret": {Secret: "1234", Email: "alice@tenant#entry"}, "nonhex-secret": {Secret: "not-a-valid-uuid-secret-xxxxxxxxx", Email: "alice@tenant#entry"}} {
		t.Run(name, func(t *testing.T) {
			if _, err := (&conf.MTProtoServerConfig{Users: []*conf.MTProtoUser{user}}).Build(); err == nil {
				t.Fatal("invalid MTProxy user was accepted")
			}
		})
	}
}
