package router

import (
	"net/http"
	"net/http/httptest"
	"sync"
	"testing"
)

func TestWebhookFireDuringSnapshotRetirement(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) { w.WriteHeader(http.StatusNoContent) }))
	defer server.Close()
	for range 10 {
		notifier, err := NewWebhookNotifier(&WebhookConfig{Url: server.URL})
		if err != nil {
			t.Fatal(err)
		}
		var workers sync.WaitGroup
		start := make(chan struct{})
		for range 4 {
			workers.Add(1)
			go func() {
				defer workers.Done()
				<-start
				for range 100 {
					notifier.Fire(snapshotContext(), "direct")
				}
			}()
		}
		workers.Add(1)
		go func() { defer workers.Done(); <-start; notifier.Close() }()
		close(start)
		workers.Wait()
		notifier.Close()
	}
}
