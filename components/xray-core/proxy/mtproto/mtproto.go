// Package mtproto implements Telegram's encrypted MTProxy ingress.
//
// Protocol behavior follows Telegram's official transport specification and
// TelegramMessenger/MTProxy implementation. See SOURCE_PROVENANCE.md.
package mtproto

import (
	"context"

	"github.com/xtls/xray-core/common"
)

const protocolName = "mtproto"

func init() {
	common.Must(common.RegisterConfig((*ServerConfig)(nil), func(ctx context.Context, config interface{}) (interface{}, error) {
		return NewServer(ctx, config.(*ServerConfig))
	}))
}
