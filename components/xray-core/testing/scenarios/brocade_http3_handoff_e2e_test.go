//go:build linux

package scenarios

import (
	"encoding/json"
	"fmt"
	"net"
	"os"
	"os/exec"
	"path/filepath"
	"testing"
	"time"

	"golang.org/x/sys/unix"
)

type frozenH3Generation struct {
	control, id string
	epoch       uint64
}

type frozenH3Status struct {
	Ready, Paused bool
	RetireReady   bool   `json:"retire_ready"`
	PublicActive  uint64 `json:"public_active"`
	Accepted      uint64
	Listeners     []struct {
		Address, Network, Role string
		BindAddress            string `json:"bind_address"`
		SocketInode            uint64 `json:"socket_inode"`
		SocketCookie           uint64 `json:"socket_cookie"`
		Protocol               int
		SocketConfigSHA256     string `json:"socket_config_sha256"`
	}
}

func (g *frozenH3Generation) rpc(t *testing.T, operation string) (frozenH3Status, []*os.File) {
	t.Helper()
	c, err := net.DialUnix("unixpacket", nil, &net.UnixAddr{Name: g.control, Net: "unixpacket"})
	if err != nil {
		t.Fatal(err)
	}
	defer c.Close()
	_ = c.SetDeadline(time.Now().Add(3 * time.Second))
	if operation != "status" {
		g.epoch++
	}
	request, _ := json.Marshal(frozenObject{"operation": operation, "generation_id": g.id, "epoch": g.epoch, "intent": fmt.Sprintf("http3-e2e-operation-%020d", g.epoch)})
	if _, err := c.Write(request); err != nil {
		t.Fatal(err)
	}
	data, ancillary := make([]byte, 65536), make([]byte, unix.CmsgSpace(16*4))
	n, oob, flags, _, err := c.ReadMsgUnix(data, ancillary)
	if err != nil || flags&(unix.MSG_TRUNC|unix.MSG_CTRUNC) != 0 {
		t.Fatalf("handoff reply: %v flags=%d", err, flags)
	}
	var response struct {
		Error  string
		Status frozenH3Status
	}
	if err := json.Unmarshal(data[:n], &response); err != nil || response.Error != "" {
		t.Fatalf("%s: %v %s", operation, err, data[:n])
	}
	var files []*os.File
	if oob > 0 {
		messages, err := unix.ParseSocketControlMessage(ancillary[:oob])
		if err != nil {
			t.Fatal(err)
		}
		for _, message := range messages {
			fds, err := unix.ParseUnixRights(&message)
			if err != nil {
				t.Fatal(err)
			}
			for _, fd := range fds {
				file := os.NewFile(uintptr(fd), "http3-inherited-listener")
				files = append(files, file)
				t.Cleanup(func() { _ = file.Close() })
			}
		}
	}
	return response.Status, files
}

func TestBrocadeFrozenHTTP3GenerationHandoff(t *testing.T) {
	frozenBinary(t)
	for _, mode := range []string{"xhttp", "masque", "masque-mixed"} {
		t.Run(mode, func(t *testing.T) {
			dir, err := os.MkdirTemp("", "h3-e2e-")
			if err != nil {
				t.Fatal(err)
			}
			t.Cleanup(func() { _ = os.RemoveAll(dir) })
			entry, api, echo := frozenPort(t), frozenPort(t), frozenEcho(t)
			frozenPacketEcho(t, echo)
			in := frozenIngress(t, "vless", entry)
			wire := frozenWire(t, "anytls", true)
			wire["tlsSettings"].(frozenObject)["alpn"] = []string{"h3"}
			wire["network"] = "xhttp"
			wire["xhttpSettings"] = frozenObject{"path": "/handoff", "mode": "stream-one"}
			outbounds := frozenFreedom()
			if mode != "xhttp" {
				in["protocol"] = "masque"
				in["settings"] = frozenObject{"users": []any{frozenObject{"email": "alice@tenant#entry", "pass": "p"}}, "address": []string{"10.14.0.1/24", "fd14::1/64"}}
				wire["network"] = "masque"
				delete(wire, "xhttpSettings")
				if mode == "masque-mixed" {
					wire["tlsSettings"].(frozenObject)["alpn"] = []string{"h2", "h3"}
				} else {
					delete(wire["tlsSettings"].(frozenObject), "alpn")
				}
				outbounds[0].(frozenObject)["settings"].(frozenObject)["redirect"] = fmt.Sprintf("127.0.0.1:%d", echo)
			}
			in["streamSettings"] = wire
			config := frozenObject{"policy": frozenPolicy(), "inbounds": []any{in}, "outbounds": outbounds}
			old := &frozenH3Generation{control: filepath.Join(dir, "old.sock"), id: "http3-old-generation-0001"}
			manifest := func(g *frozenH3Generation, listeners []any) string {
				path := filepath.Join(dir, g.id+".json")
				frozenWrite(t, path, frozenObject{"version": 2, "scope": "whole-generation", "generation_id": g.id, "control_path": g.control, "listeners": listeners})
				return path
			}
			listeners := []any{
				frozenObject{"address": fmt.Sprintf("127.0.0.1:%d", api), "network": "tcp", "role": "control", "prepare": true},
				frozenObject{"address": fmt.Sprintf("127.0.0.1:%d", entry), "network": "udp", "role": "public", "protocol": 17},
			}
			if mode == "masque-mixed" {
				listeners = append(listeners, frozenObject{"address": fmt.Sprintf("127.0.0.1:%d", entry), "network": "tcp", "role": "public"})
			}
			frozenStartProcess(t, api, nil, config, "XRAY_TCP_HANDOFF_MANIFEST="+manifest(old, listeners))
			frozenCounters(t, frozenStats(t, api), "")
			old.rpc(t, "seal")
			old.rpc(t, "resume")
			startClient := func(h2 bool) (int, *exec.Cmd) {
				port := frozenPort(t)
				settings := frozenObject{"address": "127.0.0.1", "port": entry, "id": frozenAlice, "encryption": "none"}
				clientWire := frozenWire(t, "anytls", false)
				clientWire["tlsSettings"].(frozenObject)["alpn"] = []string{"h3"}
				clientWire["network"] = "xhttp"
				clientWire["xhttpSettings"] = frozenObject{"path": "/handoff", "mode": "stream-one"}
				outbound := frozenObject{"protocol": "vless", "settings": settings, "streamSettings": clientWire}
				local := frozenDoko(port, echo)
				local["settings"].(frozenObject)["network"] = "tcp,udp"
				if mode != "xhttp" {
					outbound["protocol"] = "masque"
					outbound["settings"] = frozenObject{"address": "127.0.0.1", "port": entry}
					clientWire["network"] = "masque"
					delete(clientWire, "xhttpSettings")
					clientWire["masqueSettings"] = frozenObject{"user": "alice@tenant#entry", "pass": "p"}
					local["settings"].(frozenObject)["address"] = "10.14.0.1"
					local["settings"].(frozenObject)["port"] = 7
					if h2 {
						clientWire["tlsSettings"].(frozenObject)["alpn"] = []string{"h2"}
					}
				}
				_, cmd := frozenStart(t, frozenObject{"inbounds": []any{local}, "outbounds": []any{outbound}})
				return port, cmd
			}
			clientPort, oldClient := startClient(false)
			if err := frozenRoundtrip(frozenDirect(t, clientPort, "127.0.0.1"), []byte("old H3 payload")); err != nil {
				t.Fatal(err)
			}
			var pinned net.Conn
			var h2Client *exec.Cmd
			if mode == "masque-mixed" {
				port, cmd := startClient(true)
				h2Client = cmd
				pinned = frozenDirect(t, port, "127.0.0.1")
				if err := frozenRoundtrip(pinned, []byte("old H2 payload")); err != nil {
					t.Fatal(err)
				}
			}
			old.rpc(t, "pause")
			exported, files := old.rpc(t, "export")
			next := &frozenH3Generation{control: filepath.Join(dir, "new.sock"), id: "http3-new-generation-0002"}
			var inherited []any
			index := 0
			for _, listener := range exported.Listeners {
				endpoint := frozenObject{"address": listener.Address, "bind_address": listener.Address, "network": listener.Network, "role": listener.Role, "protocol": 17}
				if listener.Network == "tcp" {
					endpoint["fd"] = 3 + index
					endpoint["bind_address"] = listener.BindAddress
					endpoint["protocol"] = listener.Protocol
					endpoint["socket_inode"] = listener.SocketInode
					endpoint["socket_cookie"] = listener.SocketCookie
					endpoint["socket_config_sha256"] = listener.SocketConfigSHA256
					endpoint["prepare"] = listener.Role == "control"
					index++
				}
				inherited = append(inherited, endpoint)
			}
			if index != len(files) {
				t.Fatal("SCM_RIGHTS count does not match the exported TCP listener identities")
			}
			frozenStartProcess(t, api, files, config, "XRAY_TCP_HANDOFF_MANIFEST="+manifest(next, inherited))
			// A shared listener can accept a TCP connect before the child exists.
			// Its preparation API only answers after the candidate has registered
			// all listeners and completed MarkHandoffReady.
			frozenCounters(t, frozenStats(t, api), "")
			next.rpc(t, "seal")
			next.rpc(t, "resume")
			old.rpc(t, "drop_udp")
			old.rpc(t, "drop_tcp")
			if pinned != nil {
				if err := frozenRoundtrip(pinned, []byte("original H2 survives generation handoff")); err != nil {
					t.Fatal(err)
				}
			}
			freshPort, _ := startClient(false)
			if err := frozenRoundtrip(frozenDirect(t, freshPort, "127.0.0.1"), []byte("new H3 TCP payload")); err != nil {
				t.Fatal(err)
			}
			if err := frozenRoundtrip(frozenUDPDial(t, freshPort), []byte("new H3 UDP payload")); err != nil {
				t.Fatal(err)
			}
			current, _ := next.rpc(t, "status")
			if !current.Ready || current.PublicActive == 0 || current.Accepted == 0 {
				t.Fatalf("candidate did not own the new H3 traffic: %+v", current)
			}
			retiring, _ := old.rpc(t, "status")
			if pinned != nil && (retiring.RetireReady || retiring.PublicActive == 0) {
				t.Fatal("old generation lost the live original H2 connection pin")
			}
			_ = oldClient.Process.Signal(os.Interrupt)
			if h2Client != nil {
				_ = h2Client.Process.Signal(os.Interrupt)
			}
			frozenEventually(t, 3*time.Second, func() bool { status, _ := old.rpc(t, "status"); return status.PublicActive == 0 })
			retired, _ := old.rpc(t, "drain")
			if !retired.RetireReady {
				t.Fatal("closed carriers did not permit retirement")
			}
			t.Log("real old/new processes sealed and switched; SCM_RIGHTS TCP identities retained; old UDP reset; new H3 TCP/UDP exact; original mixed H2 stream and retirement pin preserved")
		})
	}
}
