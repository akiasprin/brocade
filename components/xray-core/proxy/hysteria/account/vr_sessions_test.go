package account

import (
	"github.com/xtls/xray-core/common/protocol"
	"sync/atomic"
	"testing"
)

func TestVRSessionRevocationUsesCanonicalGrant(t *testing.T) {
	v := NewValidator()
	const auth = "11111111-2222-4333-8444-555555555555"
	const alias = "11111111-2222-1200-8444-555555555555"
	account, err := (&Account{Auth: auth}).AsAccount()
	if err != nil {
		t.Fatal(err)
	}
	user := &protocol.MemoryUser{Email: "vr@example.test", Account: account}
	if err := v.Add(user); err != nil {
		t.Fatal(err)
	}
	flow := v.Get(alias)
	if flow == nil || flow.Account.(*MemoryAccount).VR != 0x1200 {
		t.Fatal("VR alias did not retain its port")
	}
	var closed atomic.Int32
	release, ok := v.RegisterConnection(flow, func() { closed.Add(1) })
	if !ok {
		t.Fatal("VR session rejected")
	}
	defer release()
	if err := v.Add(&protocol.MemoryUser{Email: "collision@example.test", Account: &MemoryAccount{Auth: alias}}); err == nil {
		t.Fatal("ambiguous VR identifier accepted")
	}
	if err := v.DelByEmail(user.Email); err != nil {
		t.Fatal(err)
	}
	if closed.Load() != 1 {
		t.Fatal("VR session survived revocation")
	}
	if err := v.Add(user); err != nil {
		t.Fatal(err)
	}
	if _, ok := v.RegisterConnection(flow, func() {}); ok {
		t.Fatal("old VR session survived regrant")
	}
	fresh := v.Get(alias)
	end, ok := v.RegisterConnection(fresh, func() {})
	if !ok {
		t.Fatal("new VR session rejected after regrant")
	}
	end()
}
