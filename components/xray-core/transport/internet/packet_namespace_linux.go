//go:build linux

package internet

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strings"
	"time"

	"golang.org/x/sys/unix"
	"google.golang.org/protobuf/proto"
)

const packetNamespaceHelper = "_brocade_udp_namespace_v1"
const packetNamespaceLimit = 64 * 1024

// A local Agent-owned policy, never a public Xray configuration or a generation
// handoff manifest. Unlisted HY2 listeners retain their original namespace.
type packetNamespacePolicy struct {
	Version       int      `json:"version"`
	NamespacePath string   `json:"namespace_path"`
	NamespaceIno  uint64   `json:"namespace_inode"`
	NamespaceDev  uint64   `json:"namespace_device"`
	BootID        string   `json:"boot_id"`
	Listeners     []string `json:"listeners"`
}

type packetNamespaceRequest struct {
	Address string `json:"address"`
	Options []byte `json:"options,omitempty"`
}

func init() {
	// A dedicated short-lived process may enter the namespace; the serving
	// process and its runtime threads must never do so. This also works in test
	// executables without a second public CLI or a persistent privileged broker.
	if len(os.Args) == 2 && os.Args[1] == packetNamespaceHelper {
		if err := runPacketNamespaceHelper(); err != nil {
			_, _ = unix.SendmsgN(3, []byte("UDP namespace socket creation failed"), nil, nil, 0)
			os.Exit(1)
		}
		os.Exit(0)
	}
}

func readPacketNamespacePolicy(path string) (packetNamespacePolicy, error) {
	var policy packetNamespacePolicy
	if !filepath.IsAbs(path) || filepath.Clean(path) != path {
		return policy, fmt.Errorf("UDP namespace policy requires a clean absolute path")
	}
	parent, err := os.Lstat(filepath.Dir(path))
	if err != nil || !parent.IsDir() || parent.Mode().Perm()&0077 != 0 {
		return policy, fmt.Errorf("UDP namespace policy directory is not private")
	}
	fd, err := unix.Open(path, unix.O_RDONLY|unix.O_CLOEXEC|unix.O_NOFOLLOW|unix.O_NONBLOCK, 0)
	if err != nil {
		return policy, fmt.Errorf("open UDP namespace policy: %w", err)
	}
	file := os.NewFile(uintptr(fd), "udp-namespace-policy")
	defer file.Close()
	var st unix.Stat_t
	if err := unix.Fstat(fd, &st); err != nil || st.Mode&unix.S_IFMT != unix.S_IFREG || st.Mode&0077 != 0 || st.Uid != uint32(os.Geteuid()) || st.Nlink != 1 {
		return policy, fmt.Errorf("UDP namespace policy identity is unsafe")
	}
	data, err := io.ReadAll(io.LimitReader(file, packetNamespaceLimit+1))
	if err != nil || len(data) > packetNamespaceLimit {
		return policy, fmt.Errorf("UDP namespace policy is unreadable or too large")
	}
	decoder := json.NewDecoder(bytes.NewReader(data))
	if err := decoder.Decode(&policy); err != nil {
		return policy, fmt.Errorf("invalid UDP namespace policy")
	}
	if decoder.Decode(new(any)) != io.EOF || policy.Version != 1 || policy.NamespaceIno == 0 || len(policy.Listeners) == 0 || len(policy.Listeners) > 512 {
		return policy, fmt.Errorf("invalid UDP namespace policy version or listener set")
	}
	if filepath.Dir(policy.NamespacePath) != "/run/netns" || filepath.Clean(policy.NamespacePath) != policy.NamespacePath || !strings.HasPrefix(filepath.Base(policy.NamespacePath), "brocade-hy2-") {
		return policy, fmt.Errorf("UDP namespace path is not Agent managed")
	}
	boot, err := os.ReadFile("/proc/sys/kernel/random/boot_id")
	if err != nil || strings.TrimSpace(string(boot)) != policy.BootID {
		return policy, fmt.Errorf("UDP namespace policy belongs to another boot")
	}
	seen := make(map[string]bool)
	for _, listener := range policy.Listeners {
		addr, err := net.ResolveUDPAddr("udp", listener)
		if err != nil || addr.IP == nil || !addr.IP.IsUnspecified() || addr.Port < 1 || addr.Zone != "" || addr.String() != listener || seen[listener] {
			return policy, fmt.Errorf("invalid UDP namespace listener")
		}
		seen[listener] = true
	}
	return policy, nil
}

func listenPacketNamespace(ctx context.Context, addr net.Addr, config *SocketConfig, logical, path string) (net.PacketConn, error) {
	if path == "" {
		return ListenSystemPacket(ctx, addr, config)
	}
	policy, err := readPacketNamespacePolicy(path)
	if err != nil {
		return nil, err
	}
	selected := false
	for _, listener := range policy.Listeners {
		selected = selected || listener == logical
	}
	if !selected {
		return ListenSystemPacket(ctx, addr, config)
	}
	if len(effectiveListener.controllers) != 0 {
		return nil, fmt.Errorf("custom listener controllers cannot be moved to a UDP namespace")
	}
	fd, err := unix.Open(policy.NamespacePath, unix.O_RDONLY|unix.O_CLOEXEC|unix.O_NOFOLLOW, 0)
	if err != nil {
		return nil, fmt.Errorf("open UDP namespace: %w", err)
	}
	namespace := os.NewFile(uintptr(fd), "udp-namespace")
	defer namespace.Close()
	var identity unix.Stat_t
	if err := unix.Fstat(fd, &identity); err != nil || identity.Ino != policy.NamespaceIno || uint64(identity.Dev) != policy.NamespaceDev {
		return nil, fmt.Errorf("UDP namespace identity changed")
	}
	options, err := proto.Marshal(config)
	if err != nil {
		return nil, fmt.Errorf("encode UDP socket options: %w", err)
	}
	request, err := json.Marshal(packetNamespaceRequest{Address: addr.String(), Options: options})
	if err != nil || len(request) > packetNamespaceLimit {
		return nil, fmt.Errorf("UDP namespace request exceeds limit")
	}
	pair, err := unix.Socketpair(unix.AF_UNIX, unix.SOCK_SEQPACKET|unix.SOCK_CLOEXEC, 0)
	if err != nil {
		return nil, err
	}
	local := os.NewFile(uintptr(pair[0]), "udp-namespace-parent")
	remote := os.NewFile(uintptr(pair[1]), "udp-namespace-child")
	defer local.Close()
	defer remote.Close()
	if err := unix.SetsockoptInt(pair[0], unix.SOL_SOCKET, unix.SO_PASSCRED, 1); err != nil {
		return nil, err
	}
	channel, err := net.FileConn(local)
	if err != nil {
		return nil, err
	}
	defer channel.Close()
	if err := channel.SetDeadline(time.Now().Add(5 * time.Second)); err != nil {
		return nil, err
	}
	bounded, cancel := context.WithTimeout(ctx, 5*time.Second)
	defer cancel()
	// /proc/self/exe pins the currently running bytes even after an atomic update.
	cmd := exec.CommandContext(bounded, "/proc/self/exe", packetNamespaceHelper)
	cmd.Env = []string{"PATH=/usr/sbin:/usr/bin:/sbin:/bin"}
	cmd.ExtraFiles = []*os.File{remote, namespace}
	if err := cmd.Start(); err != nil {
		return nil, fmt.Errorf("start UDP namespace helper: %w", err)
	}
	defer func() { cancel(); _ = cmd.Wait() }()
	if _, err := channel.Write(request); err != nil {
		return nil, fmt.Errorf("send UDP namespace request: %w", err)
	}
	control := make([]byte, unix.CmsgSpace(4)+unix.CmsgSpace(unix.SizeofUcred))
	response := make([]byte, 256)
	var n, oobn, flags int
	raw, err := channel.(*net.UnixConn).SyscallConn()
	if err != nil {
		return nil, err
	}
	var receiveErr error
	if err := raw.Read(func(fd uintptr) bool {
		n, oobn, flags, _, receiveErr = unix.Recvmsg(int(fd), response, control, unix.MSG_CMSG_CLOEXEC)
		return receiveErr != unix.EAGAIN && receiveErr != unix.EWOULDBLOCK
	}); err != nil {
		return nil, fmt.Errorf("receive UDP namespace socket: %w", err)
	}
	if receiveErr != nil {
		return nil, receiveErr
	}
	messages, err := unix.ParseSocketControlMessage(control[:oobn])
	if err != nil {
		return nil, err
	}
	var descriptors []int
	var credentials *unix.Ucred
	for _, message := range messages {
		switch message.Header.Type {
		case unix.SCM_RIGHTS:
			rights, parseErr := unix.ParseUnixRights(&message)
			if parseErr != nil {
				return nil, parseErr
			}
			descriptors = append(descriptors, rights...)
		case unix.SCM_CREDENTIALS:
			credentials, err = unix.ParseUnixCredentials(&message)
		}
	}
	defer func() {
		for _, fd := range descriptors {
			_ = unix.Close(fd)
		}
	}()
	if err != nil || flags&(unix.MSG_TRUNC|unix.MSG_CTRUNC) != 0 || string(response[:n]) != "ok" || len(descriptors) != 1 || credentials == nil || int(credentials.Pid) != cmd.Process.Pid || credentials.Uid != uint32(os.Geteuid()) {
		return nil, fmt.Errorf("UDP namespace helper returned an invalid socket response")
	}
	actualNamespace, err := unix.IoctlRetInt(descriptors[0], unix.SIOCGSKNS)
	if err != nil {
		return nil, fmt.Errorf("inspect UDP socket namespace: %w", err)
	}
	var actual unix.Stat_t
	err = unix.Fstat(actualNamespace, &actual)
	_ = unix.Close(actualNamespace)
	if err != nil || actual.Ino != identity.Ino || actual.Dev != identity.Dev {
		return nil, fmt.Errorf("UDP socket belongs to a different network namespace")
	}
	file := os.NewFile(uintptr(descriptors[0]), "udp-namespace-socket")
	conn, err := net.FilePacketConn(file)
	// Close through os.File so its finalizer cannot later close a reused FD.
	_ = file.Close()
	descriptors = nil
	if err != nil {
		return nil, err
	}
	udp, ok := conn.(*net.UDPConn)
	actualAddress, addressOK := conn.LocalAddr().(*net.UDPAddr)
	wanted, wantedOK := addr.(*net.UDPAddr)
	matches := ok && addressOK && wantedOK && actualAddress.Port == wanted.Port && actualAddress.IP.IsUnspecified()
	if matches && wanted.IP.To4() != nil && actualAddress.IP.To4() == nil {
		matches = false
		raw, inspectErr := udp.SyscallConn()
		if inspectErr == nil {
			_ = raw.Control(func(fd uintptr) {
				only, optionErr := unix.GetsockoptInt(int(fd), unix.IPPROTO_IPV6, unix.IPV6_V6ONLY)
				matches = optionErr == nil && only == 0
			})
		}
	}
	if !matches {
		_ = conn.Close()
		return nil, fmt.Errorf("UDP namespace helper bound an unexpected listener")
	}
	return conn, nil
}

func runPacketNamespaceHelper() error {
	// No serving-process work runs on this thread, and it exits without ever
	// unlocking a thread that now belongs to another namespace.
	runtime.LockOSThread()
	request := make([]byte, packetNamespaceLimit+1)
	n, _, flags, _, err := unix.Recvmsg(3, request, nil, 0)
	if err != nil || n > packetNamespaceLimit || flags&unix.MSG_TRUNC != 0 {
		return fmt.Errorf("invalid namespace helper request")
	}
	var input packetNamespaceRequest
	if err := json.Unmarshal(request[:n], &input); err != nil {
		return err
	}
	addr, err := net.ResolveUDPAddr("udp", input.Address)
	if err != nil || addr.IP == nil || !addr.IP.IsUnspecified() || addr.Port < 1 || addr.Zone != "" {
		return fmt.Errorf("invalid helper bind address")
	}
	var config SocketConfig
	if err := proto.Unmarshal(input.Options, &config); err != nil {
		return err
	}
	if err := unix.Setns(4, unix.CLONE_NEWNET); err != nil {
		return err
	}
	conn, err := ListenSystemPacket(context.Background(), addr, &config)
	if err != nil {
		return err
	}
	defer conn.Close()
	raw, err := conn.(*net.UDPConn).SyscallConn()
	if err != nil {
		return err
	}
	var sendErr error
	err = raw.Control(func(fd uintptr) {
		_, sendErr = unix.SendmsgN(3, []byte("ok"), unix.UnixRights(int(fd)), nil, 0)
	})
	if err != nil {
		return err
	}
	return sendErr
}
