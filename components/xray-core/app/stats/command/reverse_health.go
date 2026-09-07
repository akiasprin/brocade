package command

import (
	"context"
	"encoding/json"
	"github.com/xtls/xray-core/common/mux"
	"google.golang.org/grpc"
	"time"
)

func (s *statsServer) GetReverseHealthSnapshot(context.Context, *ReverseHealthRequest) (*ReverseHealthResponse, error) {
	b, err := json.Marshal(mux.GetReverseHealthSnapshot())
	return &ReverseHealthResponse{Json: b}, err
}
func (s *statsServer) WatchReverseHealth(_ *ReverseHealthRequest, stream grpc.ServerStreamingServer[ReverseHealthResponse]) error {
	ticker := time.NewTicker(100 * time.Millisecond)
	defer ticker.Stop()
	var seq uint64
	last := time.Time{}
	for {
		report := mux.GetReverseHealthSnapshot()
		if report.Sequence != seq || time.Since(last) >= 5*time.Second {
			b, err := json.Marshal(report)
			if err != nil {
				return err
			}
			if err := stream.Send(&ReverseHealthResponse{Json: b}); err != nil {
				return err
			}
			seq = report.Sequence
			last = time.Now()
		}
		select {
		case <-stream.Context().Done():
			return stream.Context().Err()
		case <-ticker.C:
		}
	}
}
