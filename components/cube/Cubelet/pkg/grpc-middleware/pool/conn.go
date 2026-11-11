// Copyright (c) 2024 Tencent Inc.
// SPDX-License-Identifier: Apache-2.0
//

package pool

import (
	"google.golang.org/grpc"
)

type Conn interface {
	Value() *grpc.ClientConn

	Close() error
}

type conn struct {
	cc   *grpc.ClientConn
	pool *pool
	once bool
}

func (c *conn) Value() *grpc.ClientConn {
	return c.cc
}

func (c *conn) Close() error {
	c.pool.decrRef()
	if c.once {
		return c.reset()
	}
	return nil
}

func (c *conn) reset() error {
	cc := c.cc
	c.cc = nil
	c.pool = nil
	c.once = false
	if cc != nil {
		return cc.Close()
	}
	return nil
}

func (p *pool) wrapConn(cc *grpc.ClientConn, once bool) *conn {
	return &conn{
		cc:   cc,
		pool: p,
		once: once,
	}
}
