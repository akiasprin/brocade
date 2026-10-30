package scenarios

import (
	"bytes"
	"crypto/sha256"
	"encoding/base64"
	"fmt"
	stdnet "net"
	"os"
	"testing"
	"time"

	"github.com/xtls/xray-core/proxy/shadowsocks_2022"
)

func TestBrocadeFrozenSS2022OversizedKeys(t *testing.T) {
	frozenBinary(t)
	for _, method := range ss2022Methods {
		t.Run(method, func(t *testing.T) {
			size := 32
			if method == shadowsocks_2022.MethodAES128GCM {
				size = 16
			}
			oversized := base64.StdEncoding.EncodeToString(bytes.Repeat([]byte{0x42}, size+17))
			t.Run("tcp", func(t *testing.T) { testShadowsocks2022Tcp(t, method, oversized) })
			t.Run("udp", func(t *testing.T) { testShadowsocks2022Udp(t, method, oversized) })
		})
	}
}

func TestBrocadeFrozenSS2022RollingMultiUserCompatibility(t *testing.T) {
	current := frozenBinary(t)
	old := os.Getenv("BROCADE_XRAY_OLD_BIN")
	if old == "" {
		t.Fatal("BROCADE_XRAY_OLD_BIN required for explicit rolling compatibility")
	}
	for _, method := range ss2022Methods[:2] {
		for _, direction := range []string{"old-client-new-server", "new-client-old-server"} {
			t.Run(method+"/"+direction, func(t *testing.T) {
				size := 32
				if method == shadowsocks_2022.MethodAES128GCM {
					size = 16
				}
				rawMaster := bytes.Repeat([]byte{0x42}, size+17)
				rawUser := bytes.Repeat([]byte{0x24}, size+9)
				master := base64.StdEncoding.EncodeToString(rawMaster)
				user := base64.StdEncoding.EncodeToString(rawUser)
				normalized := func(v []byte) string { sum := sha256.Sum256(v); return base64.StdEncoding.EncodeToString(sum[:size]) }
				serverBinary, clientBinary := current, old
				if direction == "new-client-old-server" {
					serverBinary, clientBinary = old, current
				}
				t.Setenv("BROCADE_XRAY_BIN", serverBinary)
				echo := frozenEcho(t)
				server := frozenPort(t)
				frozenStart(t, frozenObject{"policy": frozenPolicy(), "inbounds": []any{frozenObject{"tag": "ss", "listen": "127.0.0.1", "port": server, "protocol": "shadowsocks", "settings": frozenObject{"method": method, "password": master, "clients": []any{frozenObject{"password": user, "email": "alice@tenant#entry"}}, "network": "tcp,udp"}}}, "outbounds": frozenFreedom()})
				t.Setenv("BROCADE_XRAY_BIN", clientBinary)
				client := frozenPort(t)
				in := frozenDoko(client, echo)
				in["settings"].(frozenObject)["network"] = "tcp,udp"
				frozenStart(t, frozenObject{"inbounds": []any{in}, "outbounds": []any{frozenObject{"protocol": "shadowsocks", "settings": frozenObject{"address": "127.0.0.1", "port": server, "method": method, "password": normalized(rawMaster) + ":" + normalized(rawUser)}}}})
				if err := frozenRoundtrip(frozenDirect(t, client, "127.0.0.1"), bytes.Repeat([]byte{7}, 32768)); err != nil {
					t.Fatal(err)
				}
				u, err := stdnet.DialTimeout("udp", fmt.Sprintf("127.0.0.1:%d", client), time.Second)
				if err != nil {
					t.Fatal(err)
				}
				defer u.Close()
				// The fixture has a distinct UDP echo endpoint at the same target port.
				packet, err := stdnet.ListenPacket("udp", fmt.Sprintf("127.0.0.1:%d", echo))
				if err != nil {
					t.Fatal(err)
				}
				defer packet.Close()
				go func() {
					data := make([]byte, 2048)
					for {
						n, peer, err := packet.ReadFrom(data)
						if err != nil {
							return
						}
						_, _ = packet.WriteTo(data[:n], peer)
					}
				}()
				if err := frozenRoundtrip(u, bytes.Repeat([]byte{9}, 1024)); err != nil {
					t.Fatal(err)
				}
				t.Log("master/user oversized PSKs interoperate with independently SHA-256-normalized old/new client; TCP/UDP payloads exact")
			})
		}
	}
}
