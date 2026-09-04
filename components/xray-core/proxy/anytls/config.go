package anytls

import (
	"github.com/xtls/xray-core/common/errors"
	"github.com/xtls/xray-core/common/protocol"
	"google.golang.org/protobuf/proto"
)

type MemoryAccount struct {
	Password string
}

func (a *Account) AsAccount() (protocol.Account, error) {
	if a == nil || a.Password == "" {
		return nil, errors.New("anytls: empty password")
	}
	return &MemoryAccount{Password: a.Password}, nil
}

func (m *MemoryAccount) Equals(another protocol.Account) bool {
	if o, ok := another.(*MemoryAccount); ok {
		return m.Password == o.Password
	}
	return false
}

func (m *MemoryAccount) ToProto() proto.Message {
	return &Account{Password: m.Password}
}
