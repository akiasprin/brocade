package reality

import (
	"bytes"
	utls "github.com/refraction-networking/utls"
	"net"
	"slices"
	"testing"
)

func TestRandomizedGroupsMatchGeneratedKeyShares(t *testing.T) {
	for _, synthetic := range []bool{false, true} {
		t.Run(map[bool]string{false: "fixed", true: "synthetic"}[synthetic], func(t *testing.T) {
			spec, err := utls.UTLSIdToSpec(utls.HelloChrome_Auto)
			if err != nil {
				t.Fatal(err)
			}
			for _, extension := range spec.Extensions {
				if share, ok := extension.(*utls.KeyShareExtension); ok {
					share.KeyShares = []utls.KeyShare{{Group: utls.X25519}}
				}
			}
			raw, peer := net.Pipe()
			defer raw.Close()
			defer peer.Close()
			conn := utls.UClient(raw, &utls.Config{InsecureSkipVerify: true, ServerName: "fixture.example"}, utls.HelloCustom)
			if err := conn.ApplyPreset(&spec); err != nil {
				t.Fatal(err)
			}
			if err := conn.BuildHandshakeState(); err != nil {
				t.Fatal(err)
			}
			if !slices.Contains(conn.HandshakeState.Hello.SupportedCurves, utls.X25519MLKEM768) {
				t.Fatal("fixture did not advertise the hybrid group")
			}
			before := bytes.Clone(conn.HandshakeState.Hello.Raw)
			if synthetic {
				conn.ClientHelloID = utls.HelloRandomizedALPN
			}
			if err := normalizeRandomizedKeyShares(conn); err != nil {
				t.Fatal(err)
			}
			if synthetic {
				if slices.Contains(conn.HandshakeState.Hello.SupportedCurves, utls.X25519MLKEM768) {
					t.Fatal("randomized profile still advertises missing hybrid share")
				}
				if !slices.Contains(conn.HandshakeState.Hello.SupportedCurves, utls.X25519) {
					t.Fatal("classical share was removed")
				}
				if bytes.Equal(before, conn.HandshakeState.Hello.Raw) {
					t.Fatal("wire ClientHello was not regenerated")
				}
			} else if !bytes.Equal(before, conn.HandshakeState.Hello.Raw) {
				t.Fatal("fixed fingerprint was modified")
			}
		})
	}
}
