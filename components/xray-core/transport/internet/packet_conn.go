package internet

import "github.com/xtls/xray-core/common/net"

// PacketConnWrapper preserves the embedded-protocol API after the wrapper moved
// into common/net. The alias retains one runtime type and one implementation.
type PacketConnWrapper = net.PacketConnWrapper
