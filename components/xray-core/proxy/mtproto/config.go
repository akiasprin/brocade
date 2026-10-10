package mtproto

import (
	"encoding/hex"
	"strings"

	"github.com/xtls/xray-core/common/errors"
	"github.com/xtls/xray-core/common/protocol"
	"google.golang.org/protobuf/proto"
)

type MemoryAccount struct {
	Secret [16]byte
}

func (a *MemoryAccount) Equals(other protocol.Account) bool {
	account, ok := other.(*MemoryAccount)
	return ok && *a == *account
}

func (a *MemoryAccount) ToProto() proto.Message {
	return &Account{Secret: encodeSecret(a.Secret)}
}

func (a *Account) AsAccount() (protocol.Account, error) {
	secret, err := decodeSecret(a.GetSecret())
	if err != nil {
		return nil, err
	}
	return &MemoryAccount{Secret: secret}, nil
}

func decodeSecret(value string) ([16]byte, error) {
	var secret [16]byte
	canonical := strings.ReplaceAll(strings.TrimSpace(value), "-", "")
	if len(canonical) != hex.EncodedLen(len(secret)) {
		return secret, errors.New("mtproto: secret must be a 16-byte UUID or 32 hex characters")
	}
	decoded, err := hex.DecodeString(canonical)
	if err != nil {
		return secret, errors.New("mtproto: invalid secret").Base(err)
	}
	copy(secret[:], decoded)
	return secret, nil
}

// The Agent compares desired and observed credentials as strings. Always returning the
// canonical UUID form prevents an accepted 32-hex input from creating a perpetual remove/add
// reconciliation loop when GetInboundUsers serializes the in-memory account again.
func encodeSecret(secret [16]byte) string {
	compact := hex.EncodeToString(secret[:])
	return compact[:8] + "-" + compact[8:12] + "-" + compact[12:16] + "-" + compact[16:20] + "-" + compact[20:]
}
