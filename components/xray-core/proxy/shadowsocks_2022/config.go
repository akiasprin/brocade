package shadowsocks_2022

import (
	"bytes"
	"encoding/base64"

	"google.golang.org/protobuf/proto"

	"github.com/xtls/xray-core/common/protocol"
)

// MemoryAccount is an account type converted from Account.
type MemoryAccount struct {
	Key         []byte
	originalKey []byte
}

// AsAccount implements protocol.AsAccount.
func (u *Account) AsAccount() (protocol.Account, error) {
	keyStr := u.GetKey()
	raw, err := base64.StdEncoding.DecodeString(keyStr)
	if err != nil {
		raw = []byte(keyStr)
	}
	return &MemoryAccount{
		Key: raw,
	}, nil
}

// Equals implements protocol.Account.Equals().
func (a *MemoryAccount) Equals(another protocol.Account) bool {
	if account, ok := another.(*MemoryAccount); ok {
		return bytes.Equal(a.credential(), account.credential())
	}
	return false
}

func (a *MemoryAccount) ToProto() proto.Message {
	return &Account{
		Key: base64.StdEncoding.EncodeToString(a.credential()),
	}
}

func (a *MemoryAccount) credential() []byte {
	if a.originalKey != nil {
		return a.originalKey
	}
	return a.Key
}
