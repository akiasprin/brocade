package anytls

import (
	"context"
	"encoding/binary"
	"strings"
	"testing"

	"github.com/xtls/xray-core/common/buf"
	xnet "github.com/xtls/xray-core/common/net"
	"github.com/xtls/xray-core/transport"
)

const maxAnyTLSFuzzInput = 1 << 20

type fuzzDiscardWriter struct{}

func (fuzzDiscardWriter) WriteMultiBuffer(mb buf.MultiBuffer) error {
	buf.ReleaseMulti(mb)
	return nil
}

func FuzzAnyTLSSettings(f *testing.F) {
	for _, seed := range []string{
		"v=2\nclient=brocade-xray/test\npadding-md5=00000000000000000000000000000000",
		"v=1",
		"v=2\nv=1",
		"padding-md5=invalid",
		"unknown=value",
		string([]byte{'v', '=', 0xff}),
	} {
		f.Add(seed)
	}

	f.Fuzz(func(t *testing.T, input string) {
		if len(input) > maxAnyTLSFuzzInput {
			t.Skip()
		}
		settings, err := parseSettings(input)
		if err != nil {
			return
		}
		if settings.version == 0 {
			t.Fatal("successful settings parse returned protocol version 0")
		}
		if settings.paddingMD5 != "" {
			if len(settings.paddingMD5) != 32 || settings.paddingMD5 != strings.ToLower(settings.paddingMD5) {
				t.Fatalf("successful settings parse returned invalid padding MD5 %q", settings.paddingMD5)
			}
		}
	})
}

func FuzzAnyTLSPaddingScheme(f *testing.F) {
	for _, seed := range [][]byte{
		defaultPaddingScheme,
		[]byte("stop=1\n0=30-30"),
		[]byte("stop=2\n0=65535-65535\n1=64-128,c,256-512"),
		[]byte("stop=1\n0=65536-65536"),
		[]byte("stop=-1"),
		[]byte("stop=1\n0=broken"),
	} {
		f.Add(seed)
	}

	f.Fuzz(func(t *testing.T, input []byte) {
		if len(input) > maxPaddingSchemeSize+1 {
			t.Skip()
		}
		scheme, err := newPaddingScheme(input)
		if err != nil {
			return
		}
		for packet := uint32(0); packet < 16 && packet < scheme.stop; packet++ {
			for _, size := range scheme.GenerateRecordPayloadSizes(packet) {
				if size != CheckMark && (size <= 0 || size > maxPaddingTargetSize) {
					t.Fatalf("successful padding parse generated invalid size %d", size)
				}
			}
		}
	})
}

func FuzzAnyTLSSessionFrames(f *testing.F) {
	f.Add(marshalTestFrames(testWireFrame{cmd: cmdWaste, sid: 0, data: []byte("padding")}), false)
	f.Add(marshalTestFrames(testWireFrame{cmd: cmdSettings, sid: 0, data: []byte("v=2")}), false)
	f.Add(marshalTestFrames(testWireFrame{cmd: cmdHeartRequest, sid: 0}), true)
	f.Add(marshalTestFrames(testWireFrame{cmd: cmdSYN, sid: 1}, testWireFrame{cmd: cmdPSH, sid: 1, data: []byte{3, 0, 80}}), false)
	f.Add([]byte{cmdPSH, 0, 0, 0, 1, 0, 8, 1, 2}, false)
	f.Add([]byte{0xff, 0, 0, 0, 0, 0, 1, 0}, true)

	f.Fuzz(func(t *testing.T, wire []byte, isClient bool) {
		if len(wire) > maxAnyTLSFuzzInput {
			t.Skip()
		}
		session, output := newWireSession(wire, isClient)
		session.dispatcher = &testDispatcher{dispatch: func(context.Context, xnet.Destination) (*transport.Link, error) {
			return nil, context.Canceled
		}}
		if isClient {
			session.client = &Client{
				defaultPaddingScheme: getDefaultPaddingScheme(),
				authPadding:          getPadding0Size(getDefaultPaddingScheme()),
			}
		} else {
			session.server = &Server{paddingScheme: string(defaultPaddingScheme)}
		}

		_ = session.readLoop(context.Background())
		session.close(nil)
		validateFuzzFrames(t, output.Bytes())
	})
}

func validateFuzzFrames(t *testing.T, wire []byte) {
	t.Helper()
	for len(wire) > 0 {
		if len(wire) < 7 {
			t.Fatalf("implementation emitted a truncated frame header: %d bytes", len(wire))
		}
		length := int(binary.BigEndian.Uint16(wire[5:7]))
		if len(wire) < 7+length {
			t.Fatalf("implementation emitted a truncated frame body: have %d, want %d", len(wire), 7+length)
		}
		wire = wire[7+length:]
	}
}

func FuzzAnyTLSUoTRecords(f *testing.F) {
	f.Add([]byte{0, 0}, true)
	f.Add([]byte{0, 3, 'u', 'd', 'p'}, true)
	f.Add([]byte{3, 3, 'd', 'n', 's', 0, 53, 0, 0}, false)
	f.Add([]byte{0xff, 0xff, 0xff}, false)

	f.Fuzz(func(t *testing.T, input []byte, connect bool) {
		if len(input) > maxAnyTLSFuzzInput {
			t.Skip()
		}
		destination := xnet.UDPDestination(xnet.DomainAddress("fuzz.example"), 53)
		stream := newStream(1, &transport.Link{Writer: fuzzDiscardWriter{}})
		stream.uotConnect = connect
		stream.udpTarget = &destination
		session := &session{}
		_ = session.handleUDPData(stream, input)
		if len(stream.uotBuffer) > len(input) {
			t.Fatalf("UoT decoder retained %d bytes from a %d-byte input", len(stream.uotBuffer), len(input))
		}
	})
}
