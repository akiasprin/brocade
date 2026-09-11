package reverse

import (
	"context"
	"testing"

	"github.com/xtls/xray-core/common/buf"
	"github.com/xtls/xray-core/common/mux"
	"github.com/xtls/xray-core/common/net"
	"github.com/xtls/xray-core/common/session"
	"github.com/xtls/xray-core/transport"
	"github.com/xtls/xray-core/transport/pipe"
)

func newPickerClientForTest(t *testing.T) *mux.ClientWorker {
	t.Helper()
	carrierReader, carrierWriter := pipe.New(pipe.WithoutSizeLimit())
	client, err := mux.NewClientWorker(transport.Link{
		Reader: carrierReader,
		Writer: buf.Discard,
	}, mux.ClientStrategy{MaxConcurrency: 8})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		_ = carrierWriter.Close()
	})
	return client
}

func holdPickerSessionsForTest(t *testing.T, client *mux.ClientWorker, count int) {
	t.Helper()
	ctx := session.ContextWithOutbounds(context.Background(), []*session.Outbound{{
		Target: net.TCPDestination(net.LocalHostIP, 80),
	}})
	for range count {
		reader, writer := pipe.New(pipe.WithoutSizeLimit())
		if !client.Dispatch(ctx, &transport.Link{Reader: reader, Writer: buf.Discard}) {
			t.Fatal("failed to create active mux session")
		}
		t.Cleanup(func() {
			_ = writer.Close()
		})
	}
}

func TestStaticMuxPickerPrefersLeastActiveWorker(t *testing.T) {
	picker, err := NewStaticMuxPicker()
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		_ = picker.Close()
	})

	busy := newPickerClientForTest(t)
	idle := newPickerClientForTest(t)
	lessBusy := newPickerClientForTest(t)
	holdPickerSessionsForTest(t, busy, 2)
	holdPickerSessionsForTest(t, lessBusy, 1)

	picker.AddWorker(&PortalWorker{client: busy})
	picker.AddWorker(&PortalWorker{client: idle})
	picker.AddWorker(&PortalWorker{client: lessBusy})

	got, err := picker.PickAvailable()
	if err != nil {
		t.Fatal(err)
	}
	if got != idle {
		t.Fatalf("picked worker with %d active sessions, want idle worker", got.ActiveConnections())
	}
}

func TestStaticMuxPickerKeepsLegacyDrainUsableDuringHandoff(t *testing.T) {
	picker, err := NewStaticMuxPicker()
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		_ = picker.Close()
	})

	client := newPickerClientForTest(t)
	worker := &PortalWorker{client: client}
	worker.draining.Store(true)
	picker.AddWorker(worker)

	got, err := picker.PickAvailable()
	if err != nil {
		t.Fatal(err)
	}
	if got != client {
		t.Fatal("picker did not retain the draining carrier during replacement handoff")
	}
}
