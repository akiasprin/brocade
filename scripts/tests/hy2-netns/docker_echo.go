// Disposable container workload for Docker source NAT and published TCP/UDP.
package main

import (
	"fmt"
	"io"
	"net"
	"net/http"
	"os"
	"time"
)

func main() {
	if len(os.Args) == 3 && os.Args[1] == "egress" {
		client := http.Client{Timeout: 5 * time.Second}
		response, err := client.Get(os.Args[2])
		if err != nil {
			panic(err)
		}
		defer response.Body.Close()
		if response.StatusCode != http.StatusOK {
			panic(response.Status)
		}
		if _, err := io.Copy(os.Stdout, response.Body); err != nil {
			panic(err)
		}
		return
	}
	udp, err := net.ListenPacket("udp4", ":8081")
	if err != nil {
		panic(err)
	}
	go func() {
		buffer := make([]byte, 4096)
		for {
			n, peer, err := udp.ReadFrom(buffer)
			if err != nil {
				return
			}
			if _, err := udp.WriteTo(buffer[:n], peer); err != nil {
				panic(err)
			}
		}
	}()
	http.HandleFunc("/", func(w http.ResponseWriter, _ *http.Request) {
		fmt.Fprint(w, "container-ok")
	})
	panic(http.ListenAndServe(":8080", nil))
}
