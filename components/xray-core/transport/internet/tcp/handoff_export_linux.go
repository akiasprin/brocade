//go:build linux

package tcp

import (
	"encoding/json"
	"fmt"
	"io"
	"net"

	"golang.org/x/sys/unix"
)

// The status snapshot and sorted descriptors come from the same fenced export.
// Only the first packet contains status; offsets bind the remaining FD batches
// to that snapshot. The connection's original deadline covers the whole export.
func (c *tcpHandoff) writeHandoffExport(conn *net.UnixConn, response handoffResponse) error {
	total := 0
	for _, listener := range response.Status.Listeners {
		if listener.Network == "tcp" {
			total++
		}
	}
	if total > handoffMaxListeners {
		return fmt.Errorf("TCP handoff export exceeds listener limit")
	}
	for offset := 0; ; {
		end := min(offset+handoffMaxFDsPerMessage, total)
		fds, err := c.exportListenerBatch(offset, end-offset)
		if err != nil {
			return err
		}
		if len(fds) != end-offset {
			for _, fd := range fds {
				unix.Close(fd)
			}
			return fmt.Errorf("TCP handoff listener group changed during export")
		}
		frame := handoffResponse{FDExport: &handoffFDExport{Offset: offset, Total: total}}
		if offset == 0 {
			frame.Status = response.Status
		}
		err = writeHandoffPacket(conn, frame, fds)
		for _, fd := range fds {
			unix.Close(fd)
		}
		if err != nil {
			return err
		}
		if end == total {
			return nil
		}
		offset = end
	}
}

func writeHandoffPacket(conn *net.UnixConn, response handoffResponse, fds []int) error {
	if len(fds) > handoffMaxFDsPerMessage {
		return fmt.Errorf("TCP handoff packet exceeds %d descriptors", handoffMaxFDsPerMessage)
	}
	body, err := json.Marshal(response)
	if err != nil {
		return err
	}
	if len(body) > handoffMaxMessage {
		return fmt.Errorf("TCP handoff response exceeds message limit")
	}
	var rights []byte
	if len(fds) != 0 {
		rights = unix.UnixRights(fds...)
	}
	n, ancillary, err := conn.WriteMsgUnix(body, rights, nil)
	if err != nil {
		return err
	}
	if n != len(body) || ancillary != len(rights) {
		return io.ErrShortWrite
	}
	return nil
}
