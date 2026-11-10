// Copyright (c) 2026 Brocade contributors.
// SPDX-License-Identifier: Apache-2.0
package attachments

// Cubelet owns additional virtio NICs and preserves the original
// contact NIC; configured bridges belong to the machine administrator.
import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"github.com/tencentcloud/CubeSandbox/CubeNet/cubevs"
	"io"
	"net"
	"net/http"
	"net/netip"
	"os"
	"os/exec"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
	"syscall"
	"time"

	tasks "github.com/containerd/containerd/api/services/tasks/v1"
	wire "github.com/tencentcloud/CubeSandbox/pkgs/proto/networkattachments"
	"golang.org/x/sys/unix"
	"google.golang.org/grpc/metadata"
)

type bridgeSource = wire.Source
type attachment = wire.Attachment
type vmmNetwork struct {
	ID  string `json:"id"`
	Tap string `json:"tap"`
	MAC string `json:"mac"`
}
type vmmInfo struct {
	State  string `json:"state"`
	Config struct {
		Net  []vmmNetwork `json:"net"`
		CPUs struct {
			Boot uint64 `json:"boot_vcpus"`
		} `json:"cpus"`
		Memory struct {
			Size uint64 `json:"size"`
		} `json:"memory"`
	} `json:"config"`
	DeviceTree map[string]json.RawMessage `json:"device_tree"`
}

func interfaceName(name string) bool {
	if name == "" || len(name) > 15 || name == "." || name == ".." {
		return false
	}
	for _, c := range name {
		if !(c >= 'a' && c <= 'z' || c >= 'A' && c <= 'Z' || c >= '0' && c <= '9' || c == '_' || c == '-' || c == '.') {
			return false
		}
	}
	return true
}
func (s *server) validateNICs(d object) (object, error) {
	nics := array(obj(d, "spec"), "interfaces")
	if len(nics) < 1 || len(nics) > 16 {
		return nil, errors.New("invalid interface count")
	}
	if limit := number(s.cfg.Capabilities, "max_interfaces"); limit != 0 && uint64(len(nics)) > limit {
		return nil, errors.New("interface capability exceeded")
	}
	var contact object
	publicSource := ""
	for _, raw := range array(s.cfg.Capabilities, "network_sources") {
		source, ok := raw.(map[string]any)
		if ok && str(source, "kind") == "public" {
			publicSource = str(source, "id")
		}
	}
	seen := map[string]bool{}
	for _, raw := range nics {
		nic, ok := raw.(map[string]any)
		id := str(nic, "id")
		source := str(nic, "source_id")
		if !ok || !safeID.MatchString(id) || seen[id] {
			return nil, errors.New("invalid, duplicated or repeated-source interface")
		}
		seen[id] = true
		if str(nic, "purpose") == "contact" {
			if source != publicSource {
				return nil, errors.New("contact source is not the original Cube network")
			}
			if contact != nil || nic["default_ipv4"] == true || nic["default_ipv6"] == true {
				return nil, errors.New("Cube requires its single independent contact interface")
			}

			contact = nic
		} else if str(nic, "purpose") != "business" {
			return nil, errors.New("invalid NIC purpose")
		} else if _, err := allocatedNIC(nic); err != nil {
			return nil, err
		}
	}
	if contact == nil {
		return nil, errors.New("original Cube contact interface is required")
	}
	return contact, nil
}
func (s *server) vmmCall(r *record, method, action string, body any, result any) error {
	if !safeRuntime.MatchString(r.Runtime) {
		return errors.New("invalid VMM identity")
	}
	if action == "vm.add-net" || action == "vm.remove-device" {
		if err := s.verifyRunningShim(r); err != nil {
			return err
		}
		data, err := json.Marshal(body)
		if err != nil {
			return err
		}
		ctx, cancel := context.WithTimeout(s.ctx, 5*time.Second)
		defer cancel()
		ctx = metadata.AppendToOutgoingContext(ctx, "containerd-namespace", "default", "pod_scope", r.Runtime)
		command := "NetworkAttach"
		if action == "vm.remove-device" {
			command = "NetworkDetach"
		}
		_, err = s.tasks.Update(ctx, &tasks.UpdateTaskRequest{ContainerID: r.Runtime, Annotations: map[string]string{"cube.shimapi.update.action": command, "cube.shimapi.update.network": string(data)}})
		return err
	}
	root := s.cfg.VMMRoot
	if root == "" {
		root = "/run/vc/vm"
	}
	path := filepath.Join(root, r.Runtime, "chapi")
	for _, dir := range []string{root, filepath.Dir(path)} {
		st, e := os.Lstat(dir)
		if e != nil {
			return e
		}
		owner, ok := st.Sys().(*syscall.Stat_t)
		if !ok || !st.IsDir() || owner.Uid != uint32(os.Geteuid()) || st.Mode().Perm()&0022 != 0 {
			return errors.New("untrusted VMM directory")
		}
	}
	st, e := os.Lstat(path)
	if e != nil {
		return e
	}
	owner, ok := st.Sys().(*syscall.Stat_t)
	if !ok || st.Mode()&os.ModeSocket == 0 || owner.Uid != uint32(os.Geteuid()) {
		return errors.New("untrusted VMM socket")
	}
	// This socket belongs to the already ownership-checked Brocade runtime.
	if e = os.Chmod(path, 0600); e != nil {
		return e
	}
	transport := &http.Transport{DialContext: func(ctx context.Context, _, _ string) (net.Conn, error) { return trustedUnixDial(ctx, path) }}
	defer transport.CloseIdleConnections()
	client := &http.Client{Transport: transport, Timeout: 5 * time.Second, CheckRedirect: func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse }}
	var payload []byte
	if body != nil {
		payload, e = json.Marshal(body)
		if e != nil {
			return e
		}
	}
	req, e := http.NewRequestWithContext(s.ctx, method, "http://localhost/api/v1/"+action, bytes.NewReader(payload))
	if e != nil {
		return e
	}
	req.Header.Set("Content-Type", "application/json")
	response, e := client.Do(req)
	if e != nil {
		return e
	}
	defer response.Body.Close()
	data, e := io.ReadAll(io.LimitReader(response.Body, maxMessage+1))
	if e != nil || len(data) > maxMessage {
		return errors.New("VMM response exceeds bound")
	}
	if response.StatusCode < 200 || response.StatusCode >= 300 {
		return fmt.Errorf("VMM %s returned HTTP %d", action, response.StatusCode)
	}
	if result != nil {
		return json.Unmarshal(data, result)
	}
	return nil
}
func trustedUnixDial(ctx context.Context, path string) (net.Conn, error) {
	c, _, err := trustedVMMPeer(ctx, path)
	return c, err
}

func trustedVMMPeer(ctx context.Context, path string) (net.Conn, int32, error) {
	c, e := (&net.Dialer{}).DialContext(ctx, "unix", path)
	if e != nil {
		return nil, 0, e
	}
	socket, ok := c.(*net.UnixConn)
	if !ok {
		c.Close()
		return nil, 0, errors.New("not a Unix peer")
	}
	raw, e := socket.SyscallConn()
	if e != nil {
		c.Close()
		return nil, 0, e
	}
	trusted := false
	var pid int32
	e = raw.Control(func(fd uintptr) {
		cred, err := unix.GetsockoptUcred(int(fd), unix.SOL_SOCKET, unix.SO_PEERCRED)
		trusted = err == nil && cred.Uid == uint32(os.Geteuid())
		if err == nil {
			pid = cred.Pid
		}
	})
	if e != nil || !trusted {
		c.Close()
		return nil, 0, errors.New("untrusted VMM peer")
	}
	return c, pid, nil
}
func (s *server) vmInfo(r *record) (vmmInfo, error) {
	var info vmmInfo
	e := s.vmmCall(r, "GET", "vm.info", nil, &info)
	return info, e
}
func hostIP(ctx context.Context, args ...string) error {
	bounded, cancel := context.WithTimeout(ctx, 5*time.Second)
	defer cancel()
	cmd := exec.CommandContext(bounded, "ip", args...)
	cmd.Stdout = io.Discard
	cmd.Stderr = io.Discard
	if e := cmd.Run(); e != nil {
		return fmt.Errorf("host interface operation failed: %w", e)
	}
	return nil
}
func tapAlias(r *record, a *attachment) string { return "brocade-cvm:" + r.Instance + ":" + a.ID }
func checkTap(r *record, a *attachment) error {
	value, e := os.ReadFile(filepath.Join("/sys/class/net", a.Tap, "ifalias"))
	if e != nil {
		return e
	}
	if alias := strings.TrimSpace(string(value)); alias != tapAlias(r, a) {
		return errors.New("TAP belongs to a different interface")
	}
	flags, e := os.ReadFile(filepath.Join("/sys/class/net", a.Tap, "tun_flags"))
	if e != nil {
		return errors.New("owned interface is not a TAP")
	}
	n, e := strconv.ParseUint(strings.TrimSpace(string(flags)), 0, 32)
	if e != nil || n&unix.IFF_TAP == 0 {
		return errors.New("invalid owned TAP")
	}
	return nil
}
func (s *server) ensureTap(r *record, a *attachment) error {
	source := s.sourceFor(a)
	if a.Allocated != nil {
		if err := checkAssignedBridge(source, a); err != nil {
			return err
		}
	}
	if _, e := os.Stat(filepath.Join("/sys/class/net", source.Bridge, "bridge")); e != nil {
		return errors.New("administrator bridge is unavailable")
	}
	if _, e := os.Stat(filepath.Join("/sys/class/net", a.Tap)); e == nil {
		if e = checkTap(r, a); e != nil {
			return e
		}
	} else if !os.IsNotExist(e) {
		return e
	} else {
		if a.Ifindex != 0 {
			return errors.New("original TAP disappeared; refusing to substitute another device")
		}
		fd, e := unix.Open("/dev/net/tun", unix.O_RDWR|unix.O_CLOEXEC, 0)
		if e != nil {
			return e
		}
		defer unix.Close(fd)
		ifr, e := unix.NewIfreq(a.Tap)
		if e != nil {
			return e
		}
		ifr.SetUint16(unix.IFF_TAP | unix.IFF_NO_PI | unix.IFF_VNET_HDR | unix.IFF_TUN_EXCL)
		if e = unix.IoctlIfreq(fd, unix.TUNSETIFF, ifr); e != nil {
			return e
		}
		// An interrupted setup leaves an ephemeral device, automatically reclaimed
		// when this FD closes. Persistence starts only after the ownership alias.
		if e = hostIP(s.ctx, "link", "set", "dev", a.Tap, "alias", tapAlias(r, a)); e != nil {
			return e
		}
		if e = unix.IoctlSetInt(fd, unix.TUNSETPERSIST, 1); e != nil {
			return e
		}
	}
	device, e := net.InterfaceByName(a.Tap)
	if e != nil {
		return e
	}
	if a.Ifindex != 0 && a.Ifindex != device.Index {
		return errors.New("original TAP ifindex changed")
	}
	a.Ifindex = device.Index
	if e = cubevs.EnsureQuotaEndpoint(r.Instance, a.ID, uint32(device.Index)); e != nil {
		return e
	}
	if e = s.persist(r); e != nil {
		return e
	}
	if e := hostIP(s.ctx, "link", "set", "dev", a.Tap, "master", source.Bridge); e != nil {
		return e
	}
	return hostIP(s.ctx, "link", "set", "dev", a.Tap, "up")
}
func (s *server) removeTap(r *record, a *attachment) error {
	if _, e := os.Stat(filepath.Join("/sys/class/net", a.Tap)); os.IsNotExist(e) {
		return s.removeGuard(r, a)
	} else if e != nil {
		return e
	}
	if e := checkTap(r, a); e != nil {
		return e
	}
	if e := hostIP(s.ctx, "link", "delete", "dev", a.Tap); e != nil {
		return e
	}
	if _, e := os.Stat(filepath.Join("/sys/class/net", a.Tap)); !os.IsNotExist(e) {
		return errors.New("TAP deletion is not confirmed")
	}
	return s.removeGuard(r, a)
}
func (s *server) prepareNICs(r *record, d object) error {
	if r.NICs == nil {
		r.NICs = map[string]*attachment{}
	}
	for id, a := range r.NICs {
		if a.Removed && a.RemovedGeneration < number(d, "generation") {
			delete(r.NICs, id)
		}
	}
	for _, raw := range array(obj(d, "spec"), "interfaces") {
		nic := raw.(map[string]any)
		id := str(nic, "id")
		if id == r.ContactID {
			continue
		}
		if existing := r.NICs[id]; existing != nil {
			if existing.Removed || existing.Source != str(nic, "source_id") {
				return errors.New("interface identity was changed or replayed")
			}
			if err := s.applyAssigned(r, nic, existing); err != nil {
				return err
			}
			continue
		}
		identity := r.Instance + ":" + id
		sum := hash([]byte(identity))
		a := &attachment{ID: id, Source: str(nic, "source_id"), Tap: "ba" + sum[:12], DeviceID: "cvm-" + sum[:12], MAC: fmt.Sprintf("02:%s:%s:%s:%s:%s", sum[:2], sum[2:4], sum[4:6], sum[6:8], sum[8:10])}
		usedSlots := map[int]bool{}
		for _, existing := range r.NICs {
			usedSlots[existing.Slot] = true
		}
		for usedSlots[a.Slot] {
			a.Slot++
		}
		if a.Slot >= 16 {
			return errors.New("interface slot limit reached")
		}
		if err := s.applyAssigned(r, nic, a); err != nil {
			return err
		}
		r.NICs[id] = a

	}
	return s.persist(r)
}
func networkByID(info vmmInfo, id string) *vmmNetwork {
	for _, nic := range info.Config.Net {
		if nic.ID == id {
			copy := nic
			return &copy
		}
	}
	return nil
}
func (s *server) ensureNICs(r *record, d object) ([]any, object, []any, error) {
	info, e := s.vmInfo(r)
	if e != nil {
		return nil, nil, nil, e
	}
	if info.State != "Running" {
		return nil, nil, nil, errors.New("VMM is not running")
	}
	if r.ContactMAC == "" {
		for _, nic := range info.Config.Net {
			if !strings.HasPrefix(nic.ID, "cvm-") {
				if r.ContactMAC != "" {
					return nil, nil, nil, errors.New("ambiguous original contact device")
				}
				r.ContactMAC = strings.ToLower(nic.MAC)
			}
		}
		if _, e = net.ParseMAC(r.ContactMAC); e != nil {
			return nil, nil, nil, errors.New("original contact MAC is missing")
		}
	}
	if e = s.prepareNICs(r, d); e != nil {
		return nil, nil, nil, e
	}
	wanted := map[string]bool{r.ContactID: true}
	for _, raw := range array(obj(d, "spec"), "interfaces") {
		nic := raw.(map[string]any)
		id := str(nic, "id")
		wanted[id] = true
		if id == r.ContactID {
			continue
		}
		a := r.NICs[id]
		if e = s.ensureTap(r, a); e != nil {
			return nil, nil, nil, e
		}
		if e = s.ensureGuard(r, a, d); e != nil {
			return nil, nil, nil, e
		}
		if existing := networkByID(info, a.DeviceID); existing != nil {
			if existing.Tap != a.Tap || !strings.EqualFold(existing.MAC, a.MAC) {
				return nil, nil, nil, errors.New("VMM device identity differs from durable attachment")
			}
		} else if e = s.vmmCall(r, "PUT", "vm.add-net", object{"id": a.DeviceID, "tap": a.Tap, "mac": a.MAC, "num_queues": 2, "queue_size": 256}, nil); e != nil {
			return nil, nil, nil, e
		}
	}
	addresses, e := s.guest(r.Runtime, true, "/sbin/ip", "-j", "address", "show")
	if e != nil {
		return nil, nil, nil, e
	}
	observed, e := observeNICs(addresses, r, d)
	if e != nil {
		return nil, nil, nil, e
	}
	network, e := s.configureGuestNetwork(r, d, observed)
	if e != nil {
		return nil, nil, nil, e
	}
	addresses, e = s.guest(r.Runtime, true, "/sbin/ip", "-j", "address", "show")
	if e != nil {
		return nil, nil, nil, e
	}
	observed, e = observeNICs(addresses, r, d)
	if e != nil {
		return nil, nil, nil, e
	}
	// Quiesce obsolete interfaces before requesting ACPI removal. A response is
	// not an unload receipt: both device tree and Guest MAC must disappear.
	detached := []any{}
	for _, id := range sortedAttachments(r) {
		a := r.NICs[id]
		if wanted[id] {
			continue
		}
		if a.Removed {
			detached = append(detached, id)
			continue
		}
		if existing := networkByID(info, a.DeviceID); existing != nil {
			if existing.Tap != a.Tap || !strings.EqualFold(existing.MAC, a.MAC) {
				return nil, nil, nil, errors.New("obsolete VMM device has another identity")
			}
			if e = s.vmmCall(r, "PUT", "vm.remove-device", object{"id": a.DeviceID}, nil); e != nil {
				return nil, nil, nil, e
			}
		}
		current, err := s.vmInfo(r)
		if err != nil {
			return nil, nil, nil, err
		}
		if networkByID(current, a.DeviceID) != nil || current.DeviceTree[a.DeviceID] != nil {
			return nil, nil, nil, errors.New("VMM hot removal is still pending")
		}
		actual, err := s.guest(r.Runtime, true, "/sbin/ip", "-j", "address", "show")
		if err != nil {
			return nil, nil, nil, err
		}
		var devices []object
		if json.Unmarshal(actual, &devices) != nil {
			return nil, nil, nil, errors.New("Guest device inventory invalid")
		}
		for _, dev := range devices {
			if strings.EqualFold(str(dev, "address"), a.MAC) {
				return nil, nil, nil, errors.New("Guest hot removal is still pending")
			}
		}
		if e = s.removeTap(r, a); e != nil {
			return nil, nil, nil, e
		}
		a.Removed = true
		a.Ifindex = 0
		a.RemovedGeneration = number(d, "generation")
		if e = s.persist(r); e != nil {
			return nil, nil, nil, e
		}
		detached = append(detached, id)
	}
	return observed, network, detached, nil
}
func sortedAttachments(r *record) []string {
	ids := []string{}
	for id := range r.NICs {
		ids = append(ids, id)
	}
	sort.Strings(ids)
	return ids
}
func observeNICs(data []byte, r *record, d object) ([]any, error) {
	var devices []object
	if json.Unmarshal(data, &devices) != nil || len(devices) > 32 {
		return nil, errors.New("invalid Guest interface inventory")
	}
	observed := []any{}
	for _, raw := range array(obj(d, "spec"), "interfaces") {
		nic := raw.(map[string]any)
		id := str(nic, "id")
		mac := r.ContactMAC
		if id != r.ContactID {
			mac = r.NICs[id].MAC
		}
		matches := 0
		for _, dev := range devices {
			if !strings.EqualFold(str(dev, "address"), mac) {
				continue
			}
			matches++
			if !interfaceName(str(dev, "ifname")) {
				return nil, errors.New("invalid Guest device name")
			}
			addresses := []any{}
			for _, rawAddress := range array(dev, "addr_info") {
				a := rawAddress.(map[string]any)
				ip, e := netip.ParseAddr(str(a, "local"))
				if e != nil || ip.IsLoopback() || ip.IsUnspecified() {
					continue
				}
				addresses = append(addresses, fmt.Sprintf("%s/%d", ip, number(a, "prefixlen")))
			}
			observed = append(observed, object{"id": id, "device": str(dev, "ifname"), "mac": mac, "addresses": addresses, "attached": true})
		}
		if matches != 1 {
			return nil, errors.New("requested Guest virtio NIC has not appeared uniquely")
		}
	}
	return observed, nil
}
func shellWord(value string) string { return "'" + strings.ReplaceAll(value, "'", "'\"'\"'") + "'" }
func (s *server) removeAllNICs(r *record) error {
	for _, id := range sortedAttachments(r) {
		a := r.NICs[id]
		if a.Removed {
			continue
		}
		if e := s.removeTap(r, a); e != nil {
			return e
		}
		a.Ifindex = 0
		if err := s.persist(r); err != nil {
			return err
		}
	}
	return nil
}

func validateSavedNICs(r *record) error {
	if len(r.NICs) > 32 {
		return errors.New("durable NIC history exceeds limit")
	}
	seenSlots := map[int]bool{}
	for id, a := range r.NICs {
		if a == nil || id != a.ID || !safeID.MatchString(id) || !safeID.MatchString(a.Source) || a.Slot < 0 || a.Slot >= 16 || seenSlots[a.Slot] {
			return errors.New("invalid durable NIC identity")
		}
		seenSlots[a.Slot] = true
		sum := hash([]byte(r.Instance + ":" + id))
		expectedMAC := fmt.Sprintf("02:%s:%s:%s:%s:%s", sum[:2], sum[2:4], sum[4:6], sum[6:8], sum[8:10])
		if a.Allocated == nil {
			return errors.New("durable business NIC lacks a Store allocation")
		}
		{
			assigned, err := allocatedNIC(object{"source_id": a.Source, "allocated": a.Allocated})
			if err != nil || assignedPrefix(assigned.IPv4) != a.IPv4 || assignedPrefix(assigned.IPv6) != a.IPv6 {
				return errors.New("durable Store allocation is corrupt")
			}
			expectedMAC = strings.ToLower(assigned.MAC)
		}
		if a.Tap != "ba"+sum[:12] || a.DeviceID != "cvm-"+sum[:12] || a.MAC != expectedMAC || len(a.SourceSHA256) != 64 {
			return errors.New("durable NIC device binding is corrupt")
		}
		for _, value := range []string{a.IPv4, a.IPv6} {
			if value != "" {
				if _, e := netip.ParsePrefix(value); e != nil {
					return errors.New("durable NIC address is invalid")
				}
			}
		}
	}
	return nil
}
