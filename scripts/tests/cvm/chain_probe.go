// Endpoints and bounded probes used only inside the owned KVM acceptance LAN.
package main

import (
	"bytes"
	"encoding/binary"
	"encoding/json"
	"fmt"
	"io"
	"net"
	"net/http"
	"os"
	"strconv"
	"strings"
	"syscall"
	"time"
)

type Reply struct {
	Source  string `json:"source"`
	Local   string `json:"local"`
	Host    string `json:"host,omitempty"`
	Bytes   int    `json:"bytes"`
	Payload string `json:"payload,omitempty"`
}

// Linux ping datagram sockets let the kernel assign the identifier; sequence
// and a unique payload bind the reply. Guest execution uses the native root API.
func echo(address string, ipv6 bool) error {
	domain, protocol, kind, reply := syscall.AF_INET, syscall.IPPROTO_ICMP, byte(8), byte(0)
	if ipv6 {
		domain, protocol, kind, reply = syscall.AF_INET6, syscall.IPPROTO_ICMPV6, 128, 129
	}
	ip := net.ParseIP(address)
	if ip == nil {
		return fmt.Errorf("invalid ICMP destination")
	}
	fd, err := syscall.Socket(domain, syscall.SOCK_DGRAM|syscall.SOCK_CLOEXEC, protocol)
	if err != nil {
		return err
	}
	defer syscall.Close(fd)
	if err = syscall.SetsockoptTimeval(fd, syscall.SOL_SOCKET, syscall.SO_RCVTIMEO, &syscall.Timeval{Sec: 2}); err != nil {
		return err
	}
	packet := append([]byte{kind, 0, 0, 0, 0, 0, 0x12, 0x34}, []byte(fmt.Sprintf("owned-cvm-icmp-%d", time.Now().UnixNano()))...)
	if !ipv6 {
		var sum uint32
		for i := 0; i < len(packet); i += 2 {
			sum += uint32(packet[i]) << 8
			if i+1 < len(packet) {
				sum += uint32(packet[i+1])
			}
		}
		for sum>>16 != 0 {
			sum = (sum & 65535) + (sum >> 16)
		}
		binary.BigEndian.PutUint16(packet[2:4], ^uint16(sum))
	}
	var peer syscall.Sockaddr
	if ipv6 {
		v := &syscall.SockaddrInet6{}
		copy(v.Addr[:], ip.To16())
		peer = v
	} else {
		v := &syscall.SockaddrInet4{}
		copy(v.Addr[:], ip.To4())
		peer = v
	}
	if err = syscall.Sendto(fd, packet, 0, peer); err != nil {
		return err
	}
	buffer := make([]byte, 512)
	n, _, err := syscall.Recvfrom(fd, buffer, 0)
	if err != nil {
		return err
	}
	if n != len(packet) || buffer[0] != reply || buffer[1] != 0 || !bytes.Equal(buffer[6:n], packet[6:]) {
		return fmt.Errorf("ICMP echo reply mismatch")
	}
	fmt.Println(`{"icmp_echo_round_trip":true}`)
	return nil
}

func frame(w io.Writer, b []byte) error {
	if e := binary.Write(w, binary.BigEndian, uint32(len(b))); e != nil {
		return e
	}
	_, e := w.Write(b)
	return e
}
func readFrame(r io.Reader) ([]byte, error) {
	var n uint32
	if e := binary.Read(r, binary.BigEndian, &n); e != nil {
		return nil, e
	}
	if n > 1048576 {
		return nil, fmt.Errorf("oversize")
	}
	b := make([]byte, n)
	_, e := io.ReadFull(r, b)
	return b, e
}
func serve() {
	for _, family := range []string{"4", "6"} {
		host := "0.0.0.0"
		if family == "6" {
			host = "::"
		}
		for _, port := range []string{"25", "9092", "9095", "9096", "9099"} {
			listener, e := net.Listen("tcp"+family, net.JoinHostPort(host, port))
			if e != nil {
				panic(e)
			}
			go func() {
				for {
					c, e := listener.Accept()
					if e != nil {
						return
					}
					go func() {
						defer c.Close()
						_ = c.SetDeadline(time.Now().Add(10 * time.Second))
						b, e := readFrame(c)
						if e != nil {
							return
						}
						out, _ := json.Marshal(Reply{Source: c.RemoteAddr().String(), Local: c.LocalAddr().String(), Bytes: len(b), Payload: string(b)})
						_ = frame(c, out)
					}()
				}
			}()
		}
		for _, port := range []string{"53", "123", "9093", "9095", "9096", "9099"} {
			binds := strings.Split(os.Getenv("CHAIN_PROBE_UDP_BINDS"), ",")
			if len(binds) == 1 && binds[0] == "" {
				binds = []string{host}
			}
			for _, udpHost := range binds {
				parsed := net.ParseIP(udpHost)
				if parsed == nil || (parsed.To4() != nil) != (family == "4") {
					continue
				}
				conn, e := net.ListenPacket("udp"+family, net.JoinHostPort(udpHost, port))
				if e != nil {
					panic(e)
				}
				go func() {
					b := make([]byte, 65535)
					for {
						n, p, e := conn.ReadFrom(b)
						if e != nil {
							return
						}
						out, _ := json.Marshal(Reply{Source: p.String(), Local: conn.LocalAddr().String(), Bytes: n, Payload: string(b[:n])})
						_, _ = conn.WriteTo(out, p)
					}
				}()
			}
		}
		listener, e := net.Listen("tcp"+family, net.JoinHostPort(host, "8088"))
		if e != nil {
			panic(e)
		}
		server := &http.Server{ReadHeaderTimeout: 5 * time.Second, Handler: http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			b, e := io.ReadAll(io.LimitReader(r.Body, 1<<20))
			if e != nil {
				return
			}
			w.Header().Set("Content-Type", "application/json")
			_ = json.NewEncoder(w).Encode(Reply{Source: r.RemoteAddr, Host: r.Host, Bytes: len(b), Payload: string(b)})
		})}
		go func() { _ = server.Serve(listener) }()
	}
	select {}
}
func run() error {
	if len(os.Args) < 2 {
		return fmt.Errorf("mode required")
	}
	if os.Args[1] == "serve" {
		serve()
		return nil
	}
	if len(os.Args) == 3 && (os.Args[1] == "icmp4" || os.Args[1] == "icmp6") {
		return echo(os.Args[2], os.Args[1] == "icmp6")
	}
	if len(os.Args) < 4 {
		return fmt.Errorf("protocol endpoint payload-bytes/host required")
	}
	protocol, endpoint := os.Args[1], os.Args[2]
	if strings.HasPrefix(protocol, "http") {
		host := os.Args[3]
		n := 0
		if len(os.Args) > 4 {
			n, _ = strconv.Atoi(os.Args[4])
		}
		if n < 0 || n > 1048576 {
			return fmt.Errorf("oversize")
		}
		transport := &http.Transport{DialContext: (&net.Dialer{Timeout: 3 * time.Second}).DialContext, DisableKeepAlives: true}
		request, _ := http.NewRequest("POST", "http://"+endpoint+"/probe", bytes.NewReader(bytes.Repeat([]byte("x"), n)))
		request.Host = host
		response, e := (&http.Client{Transport: transport, Timeout: 7 * time.Second}).Do(request)
		if e != nil {
			return e
		}
		defer response.Body.Close()
		_, e = io.Copy(os.Stdout, io.LimitReader(response.Body, 65536))
		return e
	}
	size, e := strconv.Atoi(os.Args[3])
	if e != nil || size < 1 || size > 60000 {
		return fmt.Errorf("bad payload size")
	}
	dialer := net.Dialer{Timeout: 4 * time.Second}
	if source := os.Getenv("CHAIN_PROBE_SOURCE"); source != "" {
		ip := net.ParseIP(source)
		if ip == nil {
			return fmt.Errorf("invalid bound source")
		}
		if strings.HasPrefix(protocol, "udp") {
			dialer.LocalAddr = &net.UDPAddr{IP: ip}
		} else {
			dialer.LocalAddr = &net.TCPAddr{IP: ip}
		}
	}
	c, e := dialer.Dial(protocol, endpoint)
	if e != nil {
		return e
	}
	defer c.Close()
	_ = c.SetDeadline(time.Now().Add(15 * time.Second))
	payload := bytes.Repeat([]byte("x"), size)
	var reply []byte
	if strings.HasPrefix(protocol, "udp") {
		if _, e = c.Write(payload); e != nil {
			return e
		}
		b := make([]byte, 65535)
		var n int
		n, e = c.Read(b)
		reply = b[:n]
	} else {
		if e = frame(c, payload); e != nil {
			return e
		}
		reply, e = readFrame(c)
	}
	if e != nil {
		return e
	}
	var result Reply
	if e = json.Unmarshal(reply, &result); e != nil {
		return e
	}
	if result.Payload != string(payload) {
		return fmt.Errorf("round-trip payload mismatch: sent %d received %d", size, len(result.Payload))
	}
	result.Payload = ""
	reply, e = json.Marshal(result)
	if e != nil {
		return e
	}
	_, e = os.Stdout.Write(append(reply, '\n'))
	return e
}
func main() {
	if err := run(); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}
