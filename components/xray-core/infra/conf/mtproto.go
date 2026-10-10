package conf

import (
	"github.com/xtls/xray-core/common/errors"
	"github.com/xtls/xray-core/common/protocol"
	"github.com/xtls/xray-core/common/serial"
	"github.com/xtls/xray-core/proxy/mtproto"
	"google.golang.org/protobuf/proto"
)

type MTProtoUser struct {
	Secret string `json:"secret"`
	Level  byte   `json:"level"`
	Email  string `json:"email"`
}

type MTProtoServerConfig struct {
	Users []*MTProtoUser `json:"users"`
}

func (c *MTProtoServerConfig) Build() (proto.Message, error) {
	config := &mtproto.ServerConfig{Users: make([]*protocol.User, 0, len(c.Users))}
	for _, user := range c.Users {
		if user == nil || user.Secret == "" || user.Email == "" {
			return nil, errors.New("MTPROTO: user secret and email are required")
		}
		account := &mtproto.Account{Secret: user.Secret}
		if _, err := account.AsAccount(); err != nil {
			return nil, err
		}
		config.Users = append(config.Users, &protocol.User{
			Level:   uint32(user.Level),
			Email:   user.Email,
			Account: serial.ToTypedMessage(account),
		})
	}
	return config, nil
}
