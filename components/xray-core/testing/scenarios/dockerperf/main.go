// Command dockerperf provides the HTTPS origin and load generator used by the
// opt-in Docker protocol performance lab. It is intentionally separate from
// regular tests because throughput results depend on the host and kernel.
package main

import (
	"context"
	"crypto/tls"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"io"
	"log"
	"net"
	"net/http"
	"os"
	"runtime"
	"strconv"
	"strings"
	"sync"
	"time"
)

var payload = []byte(strings.Repeat("brocade-docker-vless-performance\n", 4096))

type result struct {
	Connections     int     `json:"connections"`
	BytesPerRequest int64   `json:"bytes_per_request"`
	Transferred     int64   `json:"transferred"`
	ElapsedSeconds  float64 `json:"elapsed_seconds"`
	GiBPerSecond    float64 `json:"gib_per_second"`
	GbitPerSecond   float64 `json:"gbit_per_second"`
	GOMAXPROCS      int     `json:"gomaxprocs"`
}

func main() {
	if len(os.Args) < 2 {
		log.Fatal("usage: dockerperf serve|load [flags]")
	}
	switch os.Args[1] {
	case "serve":
		serve(os.Args[2:])
	case "load":
		load(os.Args[2:])
	default:
		log.Fatalf("unknown mode %q", os.Args[1])
	}
}

func serve(args []string) {
	flags := flag.NewFlagSet("serve", flag.ExitOnError)
	listen := flags.String("listen", ":8443", "HTTPS listen address")
	certificate := flags.String("cert", "", "TLS certificate path")
	key := flags.String("key", "", "TLS private key path")
	_ = flags.Parse(args)
	if *certificate == "" || *key == "" {
		log.Fatal("serve requires -cert and -key")
	}

	mux := http.NewServeMux()
	mux.HandleFunc("/bytes/", func(writer http.ResponseWriter, request *http.Request) {
		count, err := strconv.ParseInt(strings.TrimPrefix(request.URL.Path, "/bytes/"), 10, 64)
		if err != nil || count <= 0 || count > 8<<30 {
			http.Error(writer, "invalid byte count", http.StatusBadRequest)
			return
		}
		writer.Header().Set("Content-Length", strconv.FormatInt(count, 10))
		writer.Header().Set("Content-Type", "application/octet-stream")
		writer.Header().Set("Connection", "close")
		for remaining := count; remaining > 0; {
			chunk := int64(len(payload))
			if chunk > remaining {
				chunk = remaining
			}
			written, err := writer.Write(payload[:chunk])
			remaining -= int64(written)
			if err != nil {
				return
			}
		}
	})

	server := &http.Server{
		Addr:              *listen,
		Handler:           mux,
		ReadHeaderTimeout: 5 * time.Second,
		TLSConfig: &tls.Config{
			MinVersion: tls.VersionTLS13,
			MaxVersion: tls.VersionTLS13,
			NextProtos: []string{"http/1.1"},
		},
	}
	log.Printf("HTTPS origin listening on %s", *listen)
	log.Fatal(server.ListenAndServeTLS(*certificate, *key))
}

func load(args []string) {
	flags := flag.NewFlagSet("load", flag.ExitOnError)
	address := flags.String("address", "172.30.0.3:8080", "client Xray address")
	connections := flags.Int("connections", 64, "parallel HTTPS downloads")
	bytesPerRequest := flags.Int64("bytes", 64<<20, "response bytes per connection")
	timeout := flags.Duration("timeout", 3*time.Minute, "whole-run timeout")
	warmup := flags.Bool("warmup", true, "perform a 1 MiB request before measurement")
	_ = flags.Parse(args)
	if *connections <= 0 || *bytesPerRequest <= 0 {
		log.Fatal("connections and bytes must be positive")
	}

	transport := &http.Transport{
		DialContext: func(ctx context.Context, _, _ string) (net.Conn, error) {
			dialer := net.Dialer{Timeout: 5 * time.Second, KeepAlive: -1}
			return dialer.DialContext(ctx, "tcp", *address)
		},
		TLSClientConfig: &tls.Config{
			InsecureSkipVerify: true, // The certificate is generated for this isolated lab.
			MinVersion:         tls.VersionTLS13,
			MaxVersion:         tls.VersionTLS13,
			ServerName:         "inner-origin.test",
			NextProtos:         []string{"http/1.1"},
		},
		DisableKeepAlives:     true,
		ForceAttemptHTTP2:     false,
		MaxConnsPerHost:       *connections,
		MaxIdleConnsPerHost:   0,
		ResponseHeaderTimeout: 15 * time.Second,
	}
	client := &http.Client{Transport: transport}
	defer transport.CloseIdleConnections()
	if *warmup {
		ctx, cancel := context.WithTimeout(context.Background(), 15*time.Second)
		err := fetch(ctx, client, 1<<20)
		cancel()
		if err != nil {
			log.Fatalf("warmup: %v", err)
		}
	}

	ctx, cancel := context.WithTimeout(context.Background(), *timeout)
	defer cancel()
	ready := make(chan struct{})
	start := make(chan struct{})
	errorsByConnection := make(chan error, *connections)
	var group sync.WaitGroup
	for index := 0; index < *connections; index++ {
		group.Add(1)
		go func(index int) {
			defer group.Done()
			ready <- struct{}{}
			<-start
			if err := fetch(ctx, client, *bytesPerRequest); err != nil {
				errorsByConnection <- fmt.Errorf("connection %d: %w", index, err)
			}
		}(index)
	}
	for index := 0; index < *connections; index++ {
		<-ready
	}
	started := time.Now()
	close(start)
	group.Wait()
	elapsed := time.Since(started)
	close(errorsByConnection)
	var joined error
	for err := range errorsByConnection {
		joined = errors.Join(joined, err)
	}
	if joined != nil {
		log.Fatal(joined)
	}

	transferred := int64(*connections) * *bytesPerRequest
	measurement := result{
		Connections:     *connections,
		BytesPerRequest: *bytesPerRequest,
		Transferred:     transferred,
		ElapsedSeconds:  elapsed.Seconds(),
		GiBPerSecond:    float64(transferred) / elapsed.Seconds() / (1 << 30),
		GbitPerSecond:   float64(transferred) * 8 / elapsed.Seconds() / 1e9,
		GOMAXPROCS:      runtime.GOMAXPROCS(0),
	}
	encoder := json.NewEncoder(os.Stdout)
	encoder.SetIndent("", "  ")
	if err := encoder.Encode(measurement); err != nil {
		log.Fatal(err)
	}
}

func fetch(ctx context.Context, client *http.Client, bodyBytes int64) error {
	request, err := http.NewRequestWithContext(ctx, http.MethodGet,
		fmt.Sprintf("https://inner-origin.test/bytes/%d", bodyBytes), nil)
	if err != nil {
		return err
	}
	request.Close = true
	response, err := client.Do(request)
	if err != nil {
		return err
	}
	defer response.Body.Close()
	if response.StatusCode != http.StatusOK {
		return fmt.Errorf("HTTP status %s", response.Status)
	}
	buffer := make([]byte, 128<<10)
	written, err := io.CopyBuffer(io.Discard, response.Body, buffer)
	if err != nil {
		return err
	}
	if written != bodyBytes {
		return fmt.Errorf("received %d bytes, want %d", written, bodyBytes)
	}
	return nil
}
