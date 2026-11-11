package mtproto

import (
	"context"
	"io"

	"github.com/xtls/xray-core/common/machinelimit"
	"github.com/xtls/xray-core/transport/internet/stat"
)

// Telegram's RPC key derivation requires real sockets, so its authenticated
// connection must enforce the budgets normally attached by the dispatcher.
// Wrapping before accounting keeps rejected bytes out of user/site counters.
type budgetConnection struct {
	stat.Connection
	ctx        context.Context
	cancel     context.CancelFunc
	controller *machinelimit.Controller
}

func (c *budgetConnection) Read(p []byte) (int, error) {
	if len(p) == 0 {
		return 0, nil
	}
	size, err := c.controller.ChunkSize()
	if err != nil {
		return 0, err
	}
	n, readErr := c.Connection.Read(p[:min(len(p), size)])
	if n > 0 {
		if err := c.controller.Wait(c.ctx, n); err != nil {
			return 0, err
		}
	}
	return n, readErr
}

func (c *budgetConnection) Write(p []byte) (int, error) {
	written := 0
	for len(p) > 0 {
		size, err := c.controller.ChunkSize()
		if err != nil {
			return written, err
		}
		part := p[:min(len(p), size)]
		if err := c.controller.Wait(c.ctx, len(part)); err != nil {
			return written, err
		}
		n, err := c.Connection.Write(part)
		written += n
		if err != nil {
			return written, err
		}
		if n != len(part) {
			return written, io.ErrShortWrite
		}
		p = p[n:]
	}
	return written, nil
}

func (c *budgetConnection) Close() error {
	// Revocation must also interrupt a pending Agent admission, not only socket I/O.
	c.cancel()
	return c.Connection.Close()
}
