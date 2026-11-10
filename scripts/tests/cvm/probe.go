// Standalone, bounded TCP/UDP client for real Guest acceptance; no Guest
// package installation or substitute network namespace is required.
package main

import (
	"bufio"
	"fmt"
	"io"
	"net"
	"os"
	"time"
)

func main() {
	if len(os.Args) == 3 && (os.Args[1] == "tcp-server" || os.Args[1] == "tcp6-server") {
		network := "tcp4"
		if os.Args[1] == "tcp6-server" {
			network = "tcp6"
		}
		server, err := net.Listen(network, os.Args[2])
		if err != nil {
			os.Exit(1)
		}
		defer server.Close()
		fmt.Println(server.Addr().(*net.TCPAddr).Port)
		for {
			c, err := server.Accept()
			if err != nil {
				return
			}
			go func() { defer c.Close(); c.SetDeadline(time.Now().Add(180 * time.Second)); io.Copy(c, c) }()
		}
	}
	if len(os.Args) == 3 && (os.Args[1] == "udp-server" || os.Args[1] == "udp6-server") {
		network := "udp4"
		if os.Args[1] == "udp6-server" {
			network = "udp6"
		}
		server, err := net.ListenPacket(network, os.Args[2])
		if err != nil {
			os.Exit(1)
		}
		defer server.Close()
		if err := server.SetDeadline(time.Now().Add(30 * time.Minute)); err != nil {
			os.Exit(1)
		}
		fmt.Println(server.LocalAddr().(*net.UDPAddr).Port)
		buffer := make([]byte, 65535)
		for {
			count, peer, err := server.ReadFrom(buffer)
			if err != nil {
				return
			}
			fmt.Fprintln(os.Stderr, "peer", peer.String())
			if _, err := server.WriteTo(buffer[:count], peer); err != nil {
				os.Exit(1)
			}
		}
	}
	if len(os.Args) != 4 && len(os.Args) != 5 {
		os.Exit(2)
	}
	dialer := net.Dialer{Timeout: 2 * time.Second}
	if len(os.Args) == 5 {
		ip := net.ParseIP(os.Args[4])
		if ip == nil {
			os.Exit(2)
		}
		if os.Args[1] == "udp4" || os.Args[1] == "udp6" {
			dialer.LocalAddr = &net.UDPAddr{IP: ip}
		} else {
			dialer.LocalAddr = &net.TCPAddr{IP: ip}
		}
	}
	network := os.Args[1]
	if network == "tcp-hold" {
		network = "tcp4"
	}
	c, e := dialer.Dial(network, os.Args[2])
	if e != nil {
		os.Exit(1)
	}
	defer c.Close()
	if e = c.SetDeadline(time.Now().Add(2 * time.Second)); e != nil {
		os.Exit(1)
	}
	if _, e = c.Write([]byte(os.Args[3])); e != nil {
		os.Exit(1)
	}
	if len(os.Args[3]) > 60000 {
		os.Exit(2)
	}
	b := make([]byte, len(os.Args[3]))
	n, e := io.ReadFull(c, b)
	if e != nil {
		os.Exit(1)
	}
	if os.Args[1] == "tcp-hold" {
		fmt.Println("READY")
		triggered := make(chan error, 1)
		go func() { _, err := bufio.NewReader(os.Stdin).ReadString('\n'); triggered <- err }()
		select {
		case err := <-triggered:
			if err != nil {
				os.Exit(1)
			}
		case <-time.After(120 * time.Second):
			os.Exit(1)
		}
		if e = c.SetDeadline(time.Now().Add(3 * time.Second)); e != nil {
			os.Exit(1)
		}
		if _, e = c.Write([]byte(os.Args[3])); e != nil {
			os.Exit(1)
		}
		n, e = io.ReadFull(c, b)
		if e != nil {
			os.Exit(1)
		}
	}
	fmt.Print(string(b[:n]))
}
