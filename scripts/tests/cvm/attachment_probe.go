// Bounded local Cubelet RPC diagnostics for the owned KVM acceptance VMs.
package main

import (
	"context"
	"encoding/json"
	"fmt"
	tasks "github.com/containerd/containerd/api/services/tasks/v1"
	"github.com/tencentcloud/CubeSandbox/pkgs/nodeexec"
	cb "github.com/tencentcloud/CubeSandbox/pkgs/proto/services/cubebox/v1"
	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials/insecure"
	"google.golang.org/grpc/metadata"
	"google.golang.org/protobuf/types/known/anypb"
	"google.golang.org/protobuf/types/known/emptypb"
	"io"
	"os"
	"path/filepath"
	"syscall"
	"time"
)

type tracedTasks struct{ tasks.TasksClient }

func (t tracedTasks) Exec(ctx context.Context, r *tasks.ExecProcessRequest, opts ...grpc.CallOption) (*emptypb.Empty, error) {
	rsp, e := t.TasksClient.Exec(ctx, r, opts...)
	if e != nil {
		fmt.Fprintln(os.Stderr, "native setup error:", e)
	}
	return rsp, e
}
func main() {
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	c, e := grpc.NewClient("unix:///run/cubelet/cubelet.sock", grpc.WithTransportCredentials(insecure.NewCredentials()))
	if e != nil {
		panic(e)
	}
	defer c.Close()
	if os.Args[1] == "guest" {
		client := nodeexec.Client{Tasks: tracedTasks{tasks.NewTasksClient(c)}, StateDir: os.Args[3]}
		args := []string{"/sbin/ip", "-j", "address", "show"}
		if len(os.Args) > 4 {
			args = os.Args[4:]
		}
		_, e := client.Exec(ctx, os.Args[2], true, args...)
		if e != nil {
			fmt.Fprintln(os.Stderr, e)
			os.Exit(1)
		}
		fmt.Println("Guest actual observation passed")
		return
	}
	if os.Args[1] == "exec" {
		ctx = metadata.AppendToOutgoingContext(ctx, "containerd-namespace", "default")
		root := "/run"
		if len(os.Args) > 3 {
			root = os.Args[3]
		}
		dir, e := os.MkdirTemp(root, "owned-kvm-exec-")
		if e != nil {
			panic(e)
		}
		defer os.RemoveAll(dir)
		for _, n := range []string{"stdin", "stdout"} {
			if e = syscall.Mkfifo(filepath.Join(dir, n), 0600); e != nil {
				panic(e)
			}
		}
		for _, name := range []string{"stdin", "stdout"} {
			f, e := os.OpenFile(filepath.Join(dir, name), os.O_RDWR|syscall.O_NONBLOCK, 0600)
			if e != nil {
				panic(e)
			}
			defer f.Close()
		}
		id := fmt.Sprintf("owned-diag-%d", time.Now().UnixNano())
		runtime := os.Args[2]
		t := tasks.NewTasksClient(c)
		spec, _ := json.Marshal(map[string]any{"terminal": true, "user": map[string]any{"uid": 0, "gid": 0}, "cwd": "/", "env": []string{"PATH=/usr/sbin:/usr/bin:/sbin:/bin"}, "args": []string{"/bin/true"}})
		_, e = t.Exec(ctx, &tasks.ExecProcessRequest{ContainerID: runtime, ExecID: id, Terminal: true, Stdin: filepath.Join(dir, "stdin"), Stdout: filepath.Join(dir, "stdout"), Spec: &anypb.Any{TypeUrl: "types.containerd.io/opencontainers/runtime-spec/1/Process", Value: spec}})
		message := ""
		if e != nil {
			message = e.Error()
		}
		json.NewEncoder(os.Stdout).Encode(map[string]any{"exec_error": message})
		t.Kill(ctx, &tasks.KillRequest{ContainerID: runtime, ExecID: id, Signal: uint32(syscall.SIGKILL)})
		t.DeleteProcess(ctx, &tasks.DeleteProcessRequest{ContainerID: runtime, ExecID: id})
		return
	}
	payload, e := io.ReadAll(io.LimitReader(os.Stdin, 128*1024+1))
	if e != nil || len(payload) > 128*1024 {
		panic("payload limit")
	}
	annotations := map[string]string{"cube.network.attachments.action": os.Args[1]}
	if len(payload) > 0 {
		annotations["cube.network.attachments.request"] = string(payload)
	}
	runtime := ""
	if len(os.Args) > 2 {
		runtime = os.Args[2]
	}
	rsp, e := cb.NewCubeboxMgrClient(c).Update(ctx, &cb.UpdateCubeSandboxRequest{RequestID: "owned-kvm-acceptance", SandboxID: runtime, Annotations: annotations})
	if e != nil {
		panic(e)
	}
	out := map[string]any{"code": rsp.GetRet().GetRetCode(), "message": rsp.GetRet().GetRetMsg()}
	if b := rsp.ExtInfo["cube.network.attachments.result"]; len(b) > 0 {
		out["result"] = json.RawMessage(b)
	}
	if e = json.NewEncoder(os.Stdout).Encode(out); e != nil {
		panic(e)
	}
}
