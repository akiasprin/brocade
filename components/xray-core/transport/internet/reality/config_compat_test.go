package reality

import (
	"google.golang.org/protobuf/proto"
	"testing"
)

func TestLegacyX25519PolicySurvivesProtoRoundTrip(t *testing.T) {
	for _, tt := range []struct {
		name  string
		value *bool
		want  bool
	}{
		{"default", nil, true}, {"strict", proto.Bool(false), false}, {"compatibility", proto.Bool(true), true},
	} {
		t.Run(tt.name, func(t *testing.T) {
			original := &Config{PrivateKey: make([]byte, 32), AllowLegacyX25519: tt.value}
			wire, err := proto.Marshal(original)
			if err != nil {
				t.Fatal(err)
			}
			decoded := new(Config)
			if err := proto.Unmarshal(wire, decoded); err != nil {
				t.Fatal(err)
			}
			if (decoded.AllowLegacyX25519 == nil) != (tt.value == nil) {
				t.Fatal("admission policy presence changed")
			}
			if got := decoded.GetREALITYConfig().AllowLegacyX25519; got != tt.want {
				t.Fatalf("allow legacy=%v want=%v", got, tt.want)
			}
		})
	}
}
