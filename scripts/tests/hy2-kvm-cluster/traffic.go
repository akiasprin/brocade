// Offline benchmark traffic. Each acknowledged byte is checked against a fixed
// pattern; upload results use the receiver's count, not socket write acceptance.
package main

import (
	"bytes"
	"encoding/binary"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"io"
	"net"
	"net/http"
	"os"
	"sync"
	"time"
)

var pattern = bytes.Repeat([]byte("brocade-kvm-integrity-0123456789ab"), 2048)

type header struct {
	Direction string
	Seconds   float64
}

func send(c net.Conn, seconds float64) (int64, error) {
	end := time.Now().Add(time.Duration(seconds * float64(time.Second)))
	var prefix [4]byte
	binary.BigEndian.PutUint32(prefix[:], uint32(len(pattern)))
	var count int64
	for time.Now().Before(end) {
		if _, err := c.Write(prefix[:]); err != nil {
			return count, err
		}
		if _, err := io.Copy(c, bytes.NewReader(pattern)); err != nil {
			return count, err
		}
		count += int64(len(pattern))
	}
	_, err := c.Write([]byte{0, 0, 0, 0})
	return count, err
}

func receive(c net.Conn) (int64, error) {
	buffer := make([]byte, len(pattern))
	var prefix [4]byte
	var count int64
	for {
		if _, err := io.ReadFull(c, prefix[:]); err != nil {
			return count, err
		}
		n := binary.BigEndian.Uint32(prefix[:])
		if n == 0 {
			return count, nil
		}
		if n != uint32(len(pattern)) {
			return count, fmt.Errorf("invalid frame length %d", n)
		}
		if _, err := io.ReadFull(c, buffer); err != nil {
			return count, err
		}
		if !bytes.Equal(buffer, pattern) {
			return count, errors.New("payload corruption")
		}
		count += int64(n)
	}
}

func serve(c net.Conn) {
	defer c.Close()
	c.SetDeadline(time.Now().Add(360 * time.Second))
	var length [4]byte
	if _, err := io.ReadFull(c, length[:]); err != nil {
		return
	}
	n := binary.BigEndian.Uint32(length[:])
	if n > 1024 {
		return
	}
	data := make([]byte, n)
	if _, err := io.ReadFull(c, data); err != nil {
		return
	}
	var h header
	if json.Unmarshal(data, &h) != nil || h.Seconds < .1 || h.Seconds > 300 {
		return
	}
	var count int64
	var err error
	if h.Direction == "download" {
		count, err = send(c, h.Seconds)
	} else if h.Direction == "upload" {
		count, err = receive(c)
	} else {
		return
	}
	if err == nil {
		json.NewEncoder(c).Encode(map[string]int64{"bytes": count})
	}
}

func dial(target, proxy string) (net.Conn, error) {
	address := target
	if proxy != "" {
		address = proxy
	}
	c, err := net.DialTimeout("tcp", address, 10*time.Second)
	if err != nil {
		return nil, err
	}
	if proxy == "" {
		return c, nil
	}
	c.SetDeadline(time.Now().Add(15 * time.Second))
	if _, err = c.Write([]byte{5, 1, 0}); err != nil {
		c.Close()
		return nil, err
	}
	var response [2]byte
	if _, err = io.ReadFull(c, response[:]); err != nil || response != [2]byte{5, 0} {
		c.Close()
		return nil, fmt.Errorf("SOCKS negotiation: %v %v", response, err)
	}
	host, port, err := net.SplitHostPort(target)
	if err != nil {
		c.Close()
		return nil, err
	}
	ip := net.ParseIP(host)
	if ip == nil {
		c.Close()
		return nil, errors.New("numeric target required")
	}
	atyp := byte(4)
	addr := ip.To16()
	if v4 := ip.To4(); v4 != nil {
		atyp = 1
		addr = v4
	}
	p, _ := net.LookupPort("tcp", port)
	request := append([]byte{5, 1, 0, atyp}, addr...)
	request = append(request, byte(p>>8), byte(p))
	if _, err = c.Write(request); err != nil {
		c.Close()
		return nil, err
	}
	var reply [4]byte
	if _, err = io.ReadFull(c, reply[:]); err != nil || reply[1] != 0 {
		c.Close()
		return nil, fmt.Errorf("SOCKS connect: %v %v", reply, err)
	}
	size := 4
	if reply[3] == 4 {
		size = 16
	} else if reply[3] != 1 {
		c.Close()
		return nil, errors.New("unexpected SOCKS family")
	}
	if _, err = io.CopyN(io.Discard, c, int64(size+2)); err != nil {
		c.Close()
		return nil, err
	}
	return c, nil
}

func main() {
	server := flag.Bool("server", false, "serve verified bulk TCP, UDP echo and HTTP health")
	target := flag.String("target", "198.18.3.2:6001", "numeric target")
	proxy := flag.String("proxy", "", "SOCKS endpoint; empty means direct")
	streams := flag.Int("streams", 1, "parallel connections (per direction for duplex)")
	seconds := flag.Float64("seconds", 10, "measured seconds")
	direction := flag.String("direction", "download", "download, upload, or duplex")
	flag.Parse()
	if *server {
		go func() {
			udp, err := net.ListenPacket("udp", ":6002")
			if err != nil {
				panic(err)
			}
			defer udp.Close()
			b := make([]byte, 65535)
			for {
				n, p, e := udp.ReadFrom(b)
				if e != nil {
					return
				}
				udp.WriteTo(b[:n], p)
			}
		}()
		go func() {
			http.HandleFunc("/", func(w http.ResponseWriter, _ *http.Request) { fmt.Fprint(w, "cluster-target-ok") })
			panic(http.ListenAndServe(":8080", nil))
		}()
		listener, err := net.Listen("tcp", ":6001")
		if err != nil {
			panic(err)
		}
		for {
			c, err := listener.Accept()
			if err != nil {
				panic(err)
			}
			go serve(c)
		}
	}
	if *streams < 1 || *streams > 128 || *seconds < .1 || *seconds > 300 {
		panic("invalid workload bounds")
	}
	start := time.Now()
	var wg sync.WaitGroup
	var mu sync.Mutex
	counts := map[string]int64{"download": 0, "upload": 0}
	failures := []string{}
	directions := []string{*direction}
	if *direction == "duplex" {
		directions = []string{"download", "upload"}
	}
	for _, d := range directions {
		for i := 0; i < *streams; i++ {
			wg.Add(1)
			go func(d string) {
				defer wg.Done()
				var count int64
				err := func() error {
					c, err := dial(*target, *proxy)
					if err != nil {
						return err
					}
					defer c.Close()
					c.SetDeadline(start.Add(time.Duration((*seconds + 25) * float64(time.Second))))
					data, _ := json.Marshal(header{d, *seconds})
					var prefix [4]byte
					binary.BigEndian.PutUint32(prefix[:], uint32(len(data)))
					if _, err = c.Write(append(prefix[:], data...)); err != nil {
						return err
					}
					if d == "download" {
						count, err = receive(c)
					} else {
						count, err = send(c, *seconds)
					}
					if err != nil {
						return err
					}
					var ack map[string]int64
					if err = json.NewDecoder(c).Decode(&ack); err != nil {
						return err
					}
					if ack["bytes"] != count {
						return errors.New("receiver byte count mismatch")
					}
					return nil
				}()
				mu.Lock()
				defer mu.Unlock()
				counts[d] += count
				if err != nil {
					failures = append(failures, err.Error())
				}
			}(d)
		}
	}
	wg.Wait()
	elapsed := time.Since(start).Seconds()
	json.NewEncoder(os.Stdout).Encode(map[string]any{"passed": len(failures) == 0, "streams_per_direction": *streams, "direction": *direction, "elapsed_seconds": elapsed, "bytes": counts, "mbps": map[string]float64{"download": float64(counts["download"]) * 8 / elapsed / 1e6, "upload": float64(counts["upload"]) * 8 / elapsed / 1e6}, "errors": failures, "verified": true})
	if len(failures) > 0 {
		os.Exit(1)
	}
}
