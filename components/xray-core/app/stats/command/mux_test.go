package command

import (
	"context"
	"encoding/json"
	"testing"

	"github.com/xtls/xray-core/common/mux"
)

func TestGetMuxSnapshotReturnsTheSharedJSONContract(t *testing.T) {
	response, err := (&statsServer{}).GetMuxSnapshot(context.Background(), &MuxRequest{})
	if err != nil {
		t.Fatal(err)
	}
	var report mux.MuxReport
	if err := json.Unmarshal(response.Json, &report); err != nil {
		t.Fatalf("decode Mux snapshot: %v", err)
	}
	if report.BootID == 0 || report.Pools == nil || report.Workers == nil || report.Events == nil {
		t.Fatalf("incomplete Mux snapshot: %+v", report)
	}
}
