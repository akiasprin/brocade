//go:build linux

package finalmask

import (
	"encoding/binary"
	"fmt"
	"math"

	"golang.org/x/sys/unix"
)

func udpSegmentSize(oob []byte) (int, error) {
	remaining := oob
	for len(remaining) > 0 {
		header, body, rest, err := unix.ParseOneSocketControlMessage(remaining)
		if err != nil {
			return 0, err
		}
		if header.Level == unix.IPPROTO_UDP && header.Type == unix.UDP_SEGMENT {
			if len(body) < 2 {
				return 0, fmt.Errorf("UDP_SEGMENT control message is too short: %d", len(body))
			}
			segmentSize := int(binary.NativeEndian.Uint16(body))
			if segmentSize == 0 {
				return 0, fmt.Errorf("invalid UDP_SEGMENT size: %d", segmentSize)
			}
			return segmentSize, nil
		}
		remaining = rest
	}
	return 0, nil
}

// adjustUDPSegmentSize accounts for the mask header added to every datagram in a GSO batch.
func adjustUDPSegmentSize(oob []byte, headerSize int) ([]byte, error) {
	segmentSize, err := udpSegmentSize(oob)
	if err != nil || segmentSize == 0 {
		return oob, err
	}
	if segmentSize+headerSize > math.MaxUint16 {
		return nil, fmt.Errorf("invalid masked UDP segment size: payload=%d header=%d", segmentSize, headerSize)
	}

	adjusted := append([]byte(nil), oob...)
	remaining := adjusted
	for len(remaining) > 0 {
		header, body, rest, parseErr := unix.ParseOneSocketControlMessage(remaining)
		if parseErr != nil {
			return nil, parseErr
		}
		if header.Level == unix.IPPROTO_UDP && header.Type == unix.UDP_SEGMENT {
			binary.NativeEndian.PutUint16(body, uint16(segmentSize+headerSize))
			return adjusted, nil
		}
		remaining = rest
	}
	return oob, nil
}
