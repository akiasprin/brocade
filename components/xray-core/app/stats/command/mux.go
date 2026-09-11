package command

import (
	"context"
	"encoding/json"

	"github.com/xtls/xray-core/common/mux"
)

func (s *statsServer) GetMuxSnapshot(context.Context, *MuxRequest) (*MuxResponse, error) {
	b, err := json.Marshal(mux.GetMuxSnapshot())
	if err != nil {
		return nil, err
	}
	return &MuxResponse{Json: b}, nil
}
