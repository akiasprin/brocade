package nodeexec

import (
	"context"
	tasks "github.com/containerd/containerd/api/services/tasks/v1"
	"github.com/containerd/containerd/v2/pkg/namespaces"
	"google.golang.org/grpc"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
	"google.golang.org/protobuf/types/known/emptypb"
	"testing"
)

type localTasks struct {
	tasks.TasksClient
	t     *testing.T
	calls []string
}

func (s *localTasks) check(ctx context.Context, method string) {
	s.t.Helper()
	namespace, err := namespaces.NamespaceRequired(ctx)
	if err != nil || namespace != "default" {
		s.t.Fatalf("%s must carry a namespace to the in-memory service: %q %v", method, namespace, err)
	}
	s.calls = append(s.calls, method)
}
func (s *localTasks) Exec(ctx context.Context, _ *tasks.ExecProcessRequest, _ ...grpc.CallOption) (*emptypb.Empty, error) {
	s.check(ctx, "exec")
	return nil, status.Error(codes.Unavailable, "controlled setup failure")
}
func (s *localTasks) Kill(ctx context.Context, _ *tasks.KillRequest, _ ...grpc.CallOption) (*emptypb.Empty, error) {
	s.check(ctx, "kill")
	return &emptypb.Empty{}, nil
}
func (s *localTasks) DeleteProcess(ctx context.Context, _ *tasks.DeleteProcessRequest, _ ...grpc.CallOption) (*tasks.DeleteResponse, error) {
	s.check(ctx, "delete")
	return &tasks.DeleteResponse{}, nil
}
func TestInMemoryNamespaceAlsoSurvivesCleanupContext(t *testing.T) {
	tasks := &localTasks{t: t}
	c := Client{Tasks: tasks, StateDir: t.TempDir()}
	if _, err := c.Exec(context.Background(), "owned-vm", true, "/sbin/ip", "-j", "address"); err == nil {
		t.Fatal("setup failure must be returned")
	}
	if len(tasks.calls) != 3 || tasks.calls[0] != "exec" || tasks.calls[1] != "kill" || tasks.calls[2] != "delete" {
		t.Fatalf("cleanup sequence: %v", tasks.calls)
	}
}
