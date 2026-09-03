package udp_test

import (
	"bytes"
	"context"
	"sync/atomic"
	"testing"
	"time"

	"github.com/xtls/xray-core/common"
	"github.com/xtls/xray-core/common/buf"
	"github.com/xtls/xray-core/common/net"
	"github.com/xtls/xray-core/common/protocol/udp"
	"github.com/xtls/xray-core/features/routing"
	"github.com/xtls/xray-core/transport"
	. "github.com/xtls/xray-core/transport/internet/udp"
	"github.com/xtls/xray-core/transport/pipe"
)

type TestDispatcher struct {
	OnDispatch func(ctx context.Context, dest net.Destination) (*transport.Link, error)
}

func (d *TestDispatcher) Dispatch(ctx context.Context, dest net.Destination) (*transport.Link, error) {
	return d.OnDispatch(ctx, dest)
}

func (d *TestDispatcher) DispatchLink(ctx context.Context, destination net.Destination, outbound *transport.Link) error {
	return nil
}

func (d *TestDispatcher) Start() error {
	return nil
}

func (d *TestDispatcher) Close() error {
	return nil
}

func (*TestDispatcher) Type() interface{} {
	return routing.DispatcherType()
}

func TestSameDestinationDispatching(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	uplinkReader, uplinkWriter := pipe.New(pipe.WithSizeLimit(1024))
	downlinkReader, downlinkWriter := pipe.New(pipe.WithSizeLimit(1024))

	go func() {
		for {
			data, err := uplinkReader.ReadMultiBuffer()
			if err != nil {
				break
			}
			err = downlinkWriter.WriteMultiBuffer(data)
			common.Must(err)
		}
	}()

	var count uint32
	td := &TestDispatcher{
		OnDispatch: func(ctx context.Context, dest net.Destination) (*transport.Link, error) {
			atomic.AddUint32(&count, 1)
			return &transport.Link{Reader: downlinkReader, Writer: uplinkWriter}, nil
		},
	}
	dest := net.UDPDestination(net.LocalHostIP, 53)

	b := buf.New()
	b.WriteString("abcd")

	var msgCount uint32
	dispatcher := NewDispatcher(td, func(ctx context.Context, packet *udp.Packet) {
		atomic.AddUint32(&msgCount, 1)
	})

	dispatcher.Dispatch(ctx, dest, b)
	for i := 0; i < 5; i++ {
		dispatcher.Dispatch(ctx, dest, b)
	}

	time.Sleep(time.Second)
	cancel()

	if count != 1 {
		t.Error("count: ", count)
	}
	if v := atomic.LoadUint32(&msgCount); v != 6 {
		t.Error("msgCount: ", v)
	}
}

func TestDispatcherConnWriteToPreservesLargeUDPPackets(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	uplinkReader, uplinkWriter := pipe.New(pipe.WithoutSizeLimit())
	downlinkReader, downlinkWriter := pipe.New(pipe.WithoutSizeLimit())
	defer uplinkWriter.Close()
	defer downlinkWriter.Close()

	conn, err := DialDispatcher(ctx, &TestDispatcher{
		OnDispatch: func(context.Context, net.Destination) (*transport.Link, error) {
			return &transport.Link{Reader: downlinkReader, Writer: uplinkWriter}, nil
		},
	})
	if err != nil {
		t.Fatal(err)
	}
	defer conn.Close()

	payload := bytes.Repeat([]byte{0x5a}, 32768)
	if n, err := conn.WriteTo(payload, &net.UDPAddr{IP: net.LocalHostIP.IP(), Port: 53}); err != nil {
		t.Fatal(err)
	} else if n != len(payload) {
		t.Fatalf("WriteTo wrote %d bytes, want %d", n, len(payload))
	}

	mb, err := uplinkReader.ReadMultiBuffer()
	if err != nil {
		t.Fatal(err)
	}
	defer buf.ReleaseMulti(mb)
	if got := mb.Len(); got != int32(len(payload)) {
		t.Fatalf("dispatched packet length = %d, want %d", got, len(payload))
	}
	got := make([]byte, len(payload))
	if n := mb.Copy(got); n != len(payload) || !bytes.Equal(got, payload) {
		t.Fatalf("dispatched packet mismatch: copied %d bytes", n)
	}
}
