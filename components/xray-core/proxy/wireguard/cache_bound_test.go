package wireguard

import (
	"fmt"
	"testing"
)

func TestDNSCacheIsBoundedAndExpires(t *testing.T) {
	var c cache
	for i := 0; i < 4100; i++ {
		c.Cache(fmt.Sprint(i), []string{"1.1.1.1"}, 60)
	}
	if len(c.m) != 4096 {
		t.Fatalf("cache size %d", len(c.m))
	}
	c.Cache("0", []string{"1.0.0.1"}, 60)
	if got := c.LookupHost("0"); len(got) != 1 || got[0] != "1.0.0.1" {
		t.Fatalf("update lost: %v", got)
	}
	c.Cache("0", []string{"1.0.0.1"}, 0)
	if got := c.LookupHost("0"); got != nil {
		t.Fatalf("expired: %v", got)
	}
}
