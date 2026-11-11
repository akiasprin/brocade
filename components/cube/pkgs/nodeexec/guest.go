// Copyright (c) 2026 Brocade contributors.
// SPDX-License-Identifier: Apache-2.0
package nodeexec

import (
	"bytes"
	"context"
	"crypto/rand"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	tasks "github.com/containerd/containerd/api/services/tasks/v1"
	"golang.org/x/sys/unix"
	"google.golang.org/grpc/metadata"
	"google.golang.org/protobuf/types/known/anypb"
	"os"
	"path/filepath"
	"syscall"
	"time"
)

// Client transports Guest I/O through Cube's native task API.
// Exec runs bounded Guest observations for Cubelet network verification.
type Client struct {
	Tasks    tasks.TasksClient
	StateDir string
}

func (s *Client) Exec(parent context.Context, runtime string, readJSON bool, args ...string) ([]byte, error) {
	ctx, cancel := context.WithTimeout(parent, 20*time.Second)
	defer cancel()
	ctx = metadata.AppendToOutgoingContext(ctx, "containerd-namespace", "default")
	dir, err := os.MkdirTemp(s.StateDir, ".guest-io-")
	if err != nil {
		return nil, errors.New("Guest output directory unavailable")
	}
	defer os.RemoveAll(dir)
	var entropy [16]byte
	if _, err = rand.Read(entropy[:]); err != nil {
		return nil, err
	}
	nonce := hex.EncodeToString(entropy[:])
	execID := "brocade-" + nonce
	stdinPath := filepath.Join(dir, "stdin")
	if err = unix.Mkfifo(stdinPath, 0600); err != nil {
		return nil, err
	}
	stdinFD, err := unix.Open(stdinPath, unix.O_RDWR|unix.O_NONBLOCK|unix.O_CLOEXEC, 0)
	if err != nil {
		return nil, err
	}
	defer unix.Close(stdinFD)
	ready := make(chan struct{})
	path := filepath.Join(dir, "stdout")
	if err = unix.Mkfifo(path, 0600); err != nil {
		return nil, errors.New("Guest output pipe unavailable")
	}
	fd, err := unix.Open(path, unix.O_RDONLY|unix.O_NONBLOCK|unix.O_CLOEXEC, 0)
	if err != nil {
		return nil, err
	}
	defer unix.Close(fd)
	// Keep an initial writer while the shim attaches its stdout relay. A task
	// can exit before that relay opens the FIFO; a complete JSON frame, not
	// premature FIFO EOF, proves observation delivery before process deletion.
	hold, err := unix.Open(path, unix.O_WRONLY|unix.O_NONBLOCK|unix.O_CLOEXEC, 0)
	if err != nil {
		return nil, err
	}
	defer unix.Close(hold)
	type capture struct {
		data []byte
		err  error
	}
	output := make(chan capture, 1)
	capturedDone := make(chan struct{})
	go func() {
		defer close(capturedDone)
		data := make([]byte, 0, 4096)
		buffer := make([]byte, 4096)
		handshaken := false
		for {
			if ctx.Err() != nil {
				output <- capture{data: data, err: ctx.Err()}
				return
			}
			n, e := unix.Read(fd, buffer)
			if n > 0 {
				if len(data)+n > 128*1024 {
					output <- capture{err: errors.New("Guest output exceeds limit")}
					return
				}
				data = append(data, buffer[:n]...)
				if !handshaken {
					end := bytes.IndexByte(data, '\n')
					if end < 0 {
						continue
					}
					if string(bytes.TrimSpace(data[:end])) != "BROCADE_READY:"+nonce {
						output <- capture{err: errors.New("Guest output readiness frame is invalid")}
						return
					}
					data = data[end+1:]
					handshaken = true
					close(ready)
				}
				marker := []byte("\nBROCADE_DONE:" + nonce + ":")
				if begin := bytes.Index(data, marker); begin >= 0 {
					tail := data[begin+len(marker):]
					if bytes.IndexByte(tail, '\n') >= 0 {
						output <- capture{data: data[:begin]}
						return
					}
				}
			}
			if n == 0 && e == nil {
				output <- capture{data: data}
				return
			}
			if e != nil && e != unix.EAGAIN && e != unix.EINTR {
				output <- capture{err: e}
				return
			}
			if n <= 0 {
				_, _ = unix.Poll([]unix.PollFd{{Fd: int32(fd), Events: unix.POLLIN | unix.POLLHUP}}, 100)
			}
		}
	}()
	defer func() { cancel(); <-capturedDone }()
	// This fixed OCI process shape never accepts a remote script or host command.
	// A readiness handshake keeps short commands alive until the shim's output
	// relay has actually attached. Guest PTY echo is disabled before the nonce.
	const handshake = `set -eu; stty -echo; printf 'BROCADE_READY:%s\n' "$1"; IFS= read -r gate; [ "$gate" = "$1" ]; token=$1; shift; set +e; "$@"; code=$?; set -e; printf '\nBROCADE_DONE:%s:%s\n' "$token" "$code"; IFS= read -r ack; [ "$ack" = "ACK:$token" ]; exit "$code"`
	wrapped := append([]string{"/bin/sh", "-c", handshake, "brocade-io", nonce}, args...)
	spec, _ := json.Marshal(map[string]any{"terminal": true, "user": map[string]any{"uid": 0, "gid": 0}, "cwd": "/", "env": []string{"PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"}, "args": wrapped})
	defer func() {
		cleanup, done := context.WithTimeout(context.Background(), 5*time.Second)
		defer done()
		cleanup = metadata.AppendToOutgoingContext(cleanup, "containerd-namespace", "default")
		_, _ = s.Tasks.Kill(cleanup, &tasks.KillRequest{ContainerID: runtime, ExecID: execID, Signal: uint32(syscall.SIGKILL)})
		_, _ = s.Tasks.DeleteProcess(cleanup, &tasks.DeleteProcessRequest{ContainerID: runtime, ExecID: execID})
	}()
	if _, err = s.Tasks.Exec(ctx, &tasks.ExecProcessRequest{ContainerID: runtime, ExecID: execID, Terminal: true, Stdin: stdinPath, Stdout: path, Spec: &anypb.Any{TypeUrl: "types.containerd.io/opencontainers/runtime-spec/1/Process", Value: spec}}); err != nil {
		return nil, errors.New("Guest execution setup failed")
	}
	if _, err = s.Tasks.Start(ctx, &tasks.StartRequest{ContainerID: runtime, ExecID: execID}); err != nil {
		return nil, errors.New("Guest execution start failed")
	}
	select {
	case <-ready:
	case <-ctx.Done():
		return nil, errors.New("Guest output readiness timed out")
	case <-output:
		return nil, errors.New("Guest output readiness failed")
	}
	if n, e := unix.Write(stdinFD, []byte(nonce+"\n")); e != nil || n != len(nonce)+1 {
		return nil, errors.New("Guest input readiness failed")
	}
	var captured capture
	select {
	case captured = <-output:
		if captured.err != nil {
			return captured.data, errors.New("Guest output capture failed")
		}
	case <-ctx.Done():
		cancel()
		<-capturedDone
		captured = <-output
		return captured.data, errors.New("Guest output capture timed out")
	}
	if n, e := unix.Write(stdinFD, []byte("ACK:"+nonce+"\n")); e != nil || n != len(nonce)+5 {
		return nil, errors.New("Guest output acknowledgement failed")
	}
	result, err := s.Tasks.Wait(ctx, &tasks.WaitRequest{ContainerID: runtime, ExecID: execID})
	if err != nil {
		return captured.data, errors.New("Guest execution receipt unavailable")
	}
	if result.ExitStatus != 0 {
		return captured.data, fmt.Errorf("Guest command exited with status %d", result.ExitStatus)
	}
	if readJSON && !json.Valid(captured.data) {
		return captured.data, errors.New("Guest observation is not complete JSON")
	}
	return captured.data, nil
}
