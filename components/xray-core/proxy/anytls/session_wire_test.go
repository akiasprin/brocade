package anytls

import (
	"context"
	"encoding/hex"
	"errors"
	"io"
	"strings"
	"testing"
)

func assertWireEOF(t *testing.T, err error) {
	t.Helper()
	if !errors.Is(err, io.EOF) {
		t.Fatalf("readLoop error = %v, want EOF after test frames", err)
	}
}

func TestSessionSettingsHandshakeAndPaddingUpdate(t *testing.T) {
	serverScheme := "stop=2\n0=64-64"
	md5 := hex.EncodeToString([]byte("client-padding!!"))
	wire := marshalTestFrames(
		testWireFrame{cmd: cmdSettings, sid: 0, data: []byte("v=2\nclient=peer\npadding-md5=" + md5)},
	)
	s, output := newWireSession(wire, false)
	s.server = &Server{paddingScheme: serverScheme}
	err := s.readLoop(context.Background())
	assertWireEOF(t, err)

	frames := parseTestFrames(t, output.Bytes())
	if len(frames) != 2 {
		t.Fatalf("response frame count = %d, want 2", len(frames))
	}
	if frames[0].cmd != cmdServerSettings || string(frames[0].data) != "v=2" {
		t.Fatalf("server settings response = %+v", frames[0])
	}
	if frames[1].cmd != cmdUpdatePaddingScheme || string(frames[1].data) != serverScheme {
		t.Fatalf("padding update response = %+v", frames[1])
	}
	if s.peerVersion != 2 || !s.handshakeDone || s.clientPaddingMD5 != md5 {
		t.Fatalf("unexpected session state: version=%d handshake=%v md5=%q", s.peerVersion, s.handshakeDone, s.clientPaddingMD5)
	}
}

func TestSessionControlFrames(t *testing.T) {
	wire := marshalTestFrames(
		testWireFrame{cmd: cmdHeartRequest, sid: 0, data: []byte("ignored")},
		testWireFrame{cmd: cmdHeartResponse, sid: 0, data: []byte("ignored")},
	)
	s, output := newWireSession(wire, false)
	err := s.readLoop(context.Background())
	assertWireEOF(t, err)
	frames := parseTestFrames(t, output.Bytes())
	if len(frames) != 1 || frames[0].cmd != cmdHeartResponse || frames[0].sid != 0 || len(frames[0].data) != 0 {
		t.Fatalf("heartbeat response = %+v", frames)
	}
}

func TestSessionClientAppliesPaddingUpdate(t *testing.T) {
	scheme := "stop=3\n0=30-30\n1=64-64"
	s, output := newWireSession(marshalTestFrames(testWireFrame{
		cmd:  cmdUpdatePaddingScheme,
		sid:  0,
		data: []byte(scheme),
	}), true)
	err := s.readLoop(context.Background())
	assertWireEOF(t, err)
	if output.Len() != 0 {
		t.Fatalf("client emitted unexpected response: %x", output.Bytes())
	}
	if s.paddingScheme == nil || string(s.paddingScheme.rawScheme) != scheme {
		t.Fatalf("client padding scheme = %q, want %q", s.paddingScheme.rawScheme, scheme)
	}
}

func TestSessionRejectsMalformedStateTransitions(t *testing.T) {
	tests := []struct {
		name     string
		isClient bool
		setup    func(*session)
		frame    testWireFrame
		wantText string
		wantOut  *testWireFrame
	}{
		{
			name:     "server-syn-before-settings",
			isClient: false,
			frame:    testWireFrame{cmd: cmdSYN, sid: 1},
			wantText: "client did not send its settings",
			wantOut:  &testWireFrame{cmd: cmdAlert, sid: 0, data: []byte("client did not send its settings")},
		},
		{
			name:     "server-duplicate-settings",
			isClient: false,
			setup: func(s *session) {
				s.handshakeDone = true
			},
			frame:    testWireFrame{cmd: cmdSettings, sid: 0, data: []byte("v=2")},
			wantText: "duplicate settings",
		},
		{
			name:     "client-settings",
			isClient: true,
			frame:    testWireFrame{cmd: cmdSettings, sid: 0, data: []byte("v=2")},
			wantText: "unexpected cmdSettings from server",
		},
		{
			name:     "server-server-settings",
			isClient: false,
			frame:    testWireFrame{cmd: cmdServerSettings, sid: 0, data: []byte("v=2")},
			wantText: "unexpected ServerSettings from client",
		},
		{
			name:     "client-syn",
			isClient: true,
			frame:    testWireFrame{cmd: cmdSYN, sid: 1},
			wantText: "unexpected SYN from server",
		},
		{
			name:     "server-empty-psh",
			isClient: false,
			setup: func(s *session) {
				s.handshakeDone = true
			},
			frame:    testWireFrame{cmd: cmdPSH, sid: 1},
			wantText: "PSH frame with empty payload",
		},
		{
			name:     "server-unknown-stream",
			isClient: false,
			setup: func(s *session) {
				s.handshakeDone = true
			},
			frame:    testWireFrame{cmd: cmdPSH, sid: 9, data: []byte("payload")},
			wantText: "received PSH for unknown stream",
		},
		{
			name:     "unknown-command",
			isClient: false,
			frame:    testWireFrame{cmd: 255, sid: 0, data: []byte("payload")},
			wantText: "unknown cmd",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			s, output := newWireSession(marshalTestFrames(tt.frame), tt.isClient)
			if tt.setup != nil {
				tt.setup(s)
			}
			err := s.readLoop(context.Background())
			if err == nil || !strings.Contains(err.Error(), tt.wantText) {
				t.Fatalf("error = %v, want text %q", err, tt.wantText)
			}
			frames := parseTestFrames(t, output.Bytes())
			if tt.wantOut == nil {
				if len(frames) != 0 {
					t.Fatalf("unexpected output frames: %+v", frames)
				}
				return
			}
			if len(frames) != 1 || frames[0].cmd != tt.wantOut.cmd || frames[0].sid != tt.wantOut.sid || string(frames[0].data) != string(tt.wantOut.data) {
				t.Fatalf("output frames = %+v, want %+v", frames, *tt.wantOut)
			}
		})
	}
}

func TestSessionSYNWithBodyReturnsSYNACKError(t *testing.T) {
	s, output := newWireSession(marshalTestFrames(
		testWireFrame{cmd: cmdSYN, sid: 7, data: []byte("unexpected")},
	), false)
	s.handshakeDone = true
	err := s.readLoop(context.Background())
	assertWireEOF(t, err)
	frames := parseTestFrames(t, output.Bytes())
	if len(frames) != 1 || frames[0].cmd != cmdSYNACK || frames[0].sid != 7 || string(frames[0].data) != "unexpected syn body" {
		t.Fatalf("SYN body response = %+v", frames)
	}
}

func TestSessionInvalidDestinationRejectsOnlyStream(t *testing.T) {
	s, output := newWireSession(marshalTestFrames(
		testWireFrame{cmd: cmdSYN, sid: 7},
		testWireFrame{cmd: cmdPSH, sid: 7, data: []byte{0xff, 0xff, 0xff}},
		testWireFrame{cmd: cmdWaste, sid: 0, data: []byte("still aligned")},
	), false)
	s.handshakeDone = true

	if err := s.readLoop(context.Background()); !errors.Is(err, io.EOF) {
		t.Fatalf("readLoop error = %v, want EOF after continuing past rejected stream", err)
	}
	frames := parseTestFrames(t, output.Bytes())
	if len(frames) != 1 || frames[0].cmd != cmdSYNACK || frames[0].sid != 7 || !strings.Contains(string(frames[0].data), "invalid destination address in SYN") {
		t.Fatalf("invalid destination response = %+v", frames)
	}
	if _, ok := s.streams[7]; ok {
		t.Fatal("rejected stream remained registered")
	}
}

func TestSessionFINClosesStream(t *testing.T) {
	s, output := newWireSession(marshalTestFrames(testWireFrame{cmd: cmdFIN, sid: 4}), false)
	st := newStream(4, nil)
	s.streams[4] = st
	err := s.readLoop(context.Background())
	assertWireEOF(t, err)
	select {
	case <-st.done:
	default:
		t.Fatal("FIN did not close stream")
	}
	if output.Len() != 0 {
		t.Fatalf("FIN emitted unexpected output: %x", output.Bytes())
	}
}
