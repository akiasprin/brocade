package reality

import (
	utls "github.com/refraction-networking/utls"
	"slices"
	"strings"
)

// Synthetic randomized profiles can advertise ML-KEM without generating its key
// share. uTLS cannot answer that group's HelloRetryRequest in this state. Match
// the advertised groups to the generated shares only for synthetic profiles;
// fixed browser fingerprints and complete hybrid handshakes keep their wire form.
func normalizeRandomizedKeyShares(conn *utls.UConn) error {
	if !strings.HasPrefix(conn.ClientHelloID.Client, utls.HelloRandomized.Client) {
		return nil
	}
	hello := conn.HandshakeState.Hello
	for _, share := range hello.KeyShares {
		if share.Group == utls.X25519MLKEM768 {
			return nil
		}
	}
	if !slices.Contains(hello.SupportedCurves, utls.X25519MLKEM768) {
		return nil
	}
	for _, extension := range conn.Extensions {
		if curves, ok := extension.(*utls.SupportedCurvesExtension); ok {
			curves.Curves = slices.DeleteFunc(slices.Clone(curves.Curves), func(group utls.CurveID) bool { return group == utls.X25519MLKEM768 })
		}
	}
	hello.SupportedCurves = slices.DeleteFunc(slices.Clone(hello.SupportedCurves), func(group utls.CurveID) bool { return group == utls.X25519MLKEM768 })
	return conn.MarshalClientHello()
}
