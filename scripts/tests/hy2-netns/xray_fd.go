// Experimental entry point only. It starts the real Xray core while substituting
// UDP descriptors supplied by the namespace helper through the existing handoff hook.
package main

import (
	"fmt"
	"net"
	"os"
	"os/signal"
	"strconv"
	"syscall"

	"github.com/xtls/xray-core/core"
	_ "github.com/xtls/xray-core/main/distro/all"
	"github.com/xtls/xray-core/transport/internet"
)

func main() {
	if len(os.Args) != 2 {
		panic("expected config file")
	}
	fds := map[string]net.PacketConn{}
	for _, family := range []string{"4", "6"} {
		value := os.Getenv("BROCADE_TEST_UDP" + family + "_FD")
		if value == "" {
			continue
		}
		fd, err := strconv.Atoi(value)
		if err != nil {
			panic(err)
		}
		file := os.NewFile(uintptr(fd), "hy2-netns-udp"+family)
		conn, err := net.FilePacketConn(file)
		file.Close()
		if err != nil {
			panic(err)
		}
		fds[family] = conn
	}
	if len(fds) != 0 {
		_, err := internet.RegisterPacketHandoffController(func(address string, _ *internet.SocketConfig, _ func(string) (net.PacketConn, error)) (net.PacketConn, internet.PacketHandoffGate, error) {
			addr, err := net.ResolveUDPAddr("udp", address)
			if err != nil {
				return nil, nil, err
			}
			family := "6"
			if addr.IP.To4() != nil {
				family = "4"
			}
			conn := fds[family]
			if conn == nil || addr.Port != 30000 {
				return nil, nil, fmt.Errorf("unexpected or duplicate listener %s", address)
			}
			delete(fds, family)
			return conn, nil, nil
		})
		if err != nil {
			panic(err)
		}
	}
	content, err := os.ReadFile(os.Args[1])
	if err != nil {
		panic(err)
	}
	instance, err := core.StartInstance("json", content)
	if err != nil {
		panic(err)
	}
	defer instance.Close()
	if len(fds) != 0 {
		panic("one or more inherited sockets were not consumed")
	}
	fmt.Println("BROCADE_NETNS_XRAY_READY")
	shutdown := make(chan os.Signal, 1)
	signal.Notify(shutdown, os.Interrupt, syscall.SIGTERM)
	<-shutdown
}
