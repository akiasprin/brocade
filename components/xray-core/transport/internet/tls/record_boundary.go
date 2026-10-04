package tls

import (
	"encoding/binary"

	"github.com/xtls/xray-core/common/net"
)

// tlsRecordBoundaryConn prevents the TLS handshake reader from consuming the
// first application record into crypto/tls' private read-ahead buffer. It does
// not buffer bytes itself: each underlying read stops at a TLS record edge.
type tlsRecordBoundaryConn struct {
	net.Conn
	header    [5]byte
	headerLen int
	remaining int
}

func newTLSRecordBoundaryConn(conn net.Conn) *tlsRecordBoundaryConn {
	return &tlsRecordBoundaryConn{Conn: conn}
}

func (c *tlsRecordBoundaryConn) Read(p []byte) (int, error) {
	if len(p) == 0 {
		return 0, nil
	}
	limit := len(p)
	if c.headerLen < len(c.header) {
		if left := len(c.header) - c.headerLen; limit > left {
			limit = left
		}
	} else if limit > c.remaining {
		limit = c.remaining
	}

	n, err := c.Conn.Read(p[:limit])
	if c.headerLen < len(c.header) {
		copy(c.header[c.headerLen:], p[:n])
		c.headerLen += n
		if c.headerLen == len(c.header) {
			c.remaining = int(binary.BigEndian.Uint16(c.header[3:5]))
			if c.remaining == 0 {
				c.headerLen = 0
			}
		}
	} else {
		c.remaining -= n
		if c.remaining == 0 {
			c.headerLen = 0
		}
	}
	return n, err
}
