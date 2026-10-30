package reality

import "testing"

func TestBrocadeLegacyX25519Admission(t *testing.T) {
	key := make([]byte, 32)
	for _, tt := range []struct {
		name   string
		groups []CurveID
		shares []keyShare
		want   bool
	}{
		{"classical", []CurveID{X25519}, []keyShare{{group: X25519, data: key}}, true},
		{"grease", []CurveID{CurveID(0x0a0a), X25519}, []keyShare{{group: CurveID(0x0a0a), data: []byte{0}}, {group: X25519, data: key}}, true},
		{"stripped_hybrid", []CurveID{X25519MLKEM768, X25519}, []keyShare{{group: X25519, data: key}}, false},
		{"unadvertised_hybrid", []CurveID{X25519}, []keyShare{{group: X25519MLKEM768, data: key}, {group: X25519, data: key}}, false},
		{"duplicate_classical", []CurveID{X25519}, []keyShare{{group: X25519, data: key}, {group: X25519, data: key}}, false},
		{"truncated_classical", []CurveID{X25519}, []keyShare{{group: X25519, data: key[:31]}}, false},
		{"unadvertised_classical", nil, []keyShare{{group: X25519, data: key}}, false},
		{"no_key_share", []CurveID{X25519}, nil, false},
	} {
		t.Run(tt.name, func(t *testing.T) {
			got := legacyX25519KeyShare(&clientHelloMsg{supportedCurves: tt.groups, keyShares: tt.shares})
			if (got != nil) != tt.want {
				t.Fatalf("accepted=%v want=%v", got != nil, tt.want)
			}
		})
	}
}
func TestBrocadeLegacyConfigClone(t *testing.T) {
	for _, enabled := range []bool{false, true} {
		c := &Config{AllowLegacyX25519: enabled}
		if c.Clone().AllowLegacyX25519 != enabled {
			t.Fatal("clone changed admission policy")
		}
	}
}
