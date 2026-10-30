package blackhole_test

import (
	"bufio"
	"bytes"
	"context"
	"crypto/rand"
	"net/http"
	"testing"
	"time"

	"github.com/xtls/xray-core/common"
	"github.com/xtls/xray-core/common/buf"
	"github.com/xtls/xray-core/common/session"
	"github.com/xtls/xray-core/proxy/blackhole"
	"github.com/xtls/xray-core/transport"
	"github.com/xtls/xray-core/transport/pipe"
)

func TestBlackholeHTTPResponse(t *testing.T) {
	ctx := session.ContextWithOutbounds(context.Background(), []*session.Outbound{{}})
	handler, err := blackhole.New(ctx, &blackhole.Config{
		Response: &blackhole.Response{Type: "http"},
	})
	common.Must(err)

	reader, writer := pipe.New(pipe.WithoutSizeLimit())
	defer reader.Interrupt()

	var mb buf.MultiBuffer
	var rerr error
	done := make(chan struct{})
	go func() {
		defer close(done)
		b, e := reader.ReadMultiBuffer()
		mb = b
		rerr = e
	}()
	link := transport.Link{
		Reader: reader,
		Writer: writer,
	}
	common.Must(handler.Process(ctx, &link, nil))
	select {
	case <-done:
	case <-time.After(5 * time.Second):
		t.Fatal("timed out reading blackhole response")
	}
	if rerr != nil {
		t.Fatal(rerr)
	}
	defer buf.ReleaseMulti(mb)
	data := make([]byte, mb.Len())
	mb.Copy(data)
	resp := common.Must2(http.ReadResponse(bufio.NewReader(bytes.NewBuffer(data)), nil))
	if resp.StatusCode != 403 {
		t.Errorf("expected 403 response, got %d", resp.StatusCode)
	}
}

func TestBlackholeCustomResponse(t *testing.T) {
	ctx := session.ContextWithOutbounds(context.Background(), []*session.Outbound{{}})
	// slightly bigger than a buffer
	expected := make([]byte, buf.Size+1000)
	if _, err := rand.Read(expected); err != nil {
		t.Fatal(err)
	}
	handler, err := blackhole.New(ctx, &blackhole.Config{
		Response: &blackhole.Response{
			Type:               "custom",
			CustomResponseData: expected,
		},
	})
	common.Must(err)

	reader, writer := pipe.New(pipe.WithoutSizeLimit())
	defer reader.Interrupt()
	var actual buf.MultiBuffer
	var rerr error
	done := make(chan struct{})
	go func() {
		defer close(done)
		actual, rerr = reader.ReadMultiBuffer()
	}()

	link := transport.Link{Reader: reader, Writer: writer}
	common.Must(handler.Process(ctx, &link, nil))
	select {
	case <-done:
	case <-time.After(5 * time.Second):
		t.Fatal("timed out reading custom blackhole response")
	}
	common.Must(rerr)
	defer buf.ReleaseMulti(actual)

	if actual.String() != string(expected) {
		t.Errorf("custom response mismatch")
	}
}
