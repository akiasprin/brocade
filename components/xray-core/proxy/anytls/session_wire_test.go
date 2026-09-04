package anytls

import (
	"context"
	"encoding/binary"
	"encoding/hex"
	"errors"
	"fmt"
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
		testWireFrame{cmd: cmdHeartRequest, sid: 0},
		testWireFrame{cmd: cmdHeartResponse, sid: 0},
	)
	s, output := newWireSession(wire, false)
	err := s.readLoop(context.Background())
	assertWireEOF(t, err)
	frames := parseTestFrames(t, output.Bytes())
	if len(frames) != 1 || frames[0].cmd != cmdHeartResponse || frames[0].sid != 0 || len(frames[0].data) != 0 {
		t.Fatalf("heartbeat response = %+v", frames)
	}
}

func TestSessionClientAppliesPaddingUpdateToFutureSessions(t *testing.T) {
	scheme := "stop=3\n0=30-30\n1=64-64"
	s, output := newWireSession(marshalTestFrames(testWireFrame{
		cmd:  cmdUpdatePaddingScheme,
		sid:  0,
		data: []byte(scheme),
	}), true)
	currentScheme := s.paddingScheme
	client := &Client{
		defaultPaddingScheme: currentScheme,
		authPadding:          getPadding0Size(currentScheme),
	}
	s.client = client
	err := s.readLoop(context.Background())
	assertWireEOF(t, err)
	if output.Len() != 0 {
		t.Fatalf("client emitted unexpected response: %x", output.Bytes())
	}
	if s.paddingScheme != currentScheme {
		t.Fatal("padding update changed the current session snapshot")
	}
	updatedScheme, authPadding := client.paddingSnapshot()
	if updatedScheme == nil || string(updatedScheme.rawScheme) != scheme {
		t.Fatalf("future session padding scheme = %q, want %q", updatedScheme.rawScheme, scheme)
	}
	if want := getPadding0Size(updatedScheme); authPadding != want {
		t.Fatalf("future auth padding = %d, want %d", authPadding, want)
	}
}

func TestSessionRejectsOversizedPacketZeroPaddingUpdate(t *testing.T) {
	currentScheme, err := parsePaddingScheme("stop=1\n0=30-30")
	if err != nil {
		t.Fatal(err)
	}
	client := &Client{
		defaultPaddingScheme: currentScheme,
		authPadding:          getPadding0Size(currentScheme),
	}
	s, _ := newWireSession(marshalTestFrames(testWireFrame{
		cmd:  cmdUpdatePaddingScheme,
		sid:  0,
		data: []byte("stop=1\n0=65536-65536"),
	}), true)
	s.client = client

	err = s.readLoop(context.Background())
	if err == nil || !strings.Contains(err.Error(), "invalid padding update") {
		t.Fatalf("readLoop error = %v, want invalid padding update", err)
	}
	scheme, authPadding := client.paddingSnapshot()
	if scheme != currentScheme || authPadding != 30 {
		t.Fatalf("invalid update changed last-good padding: scheme=%p want=%p auth=%d", scheme, currentScheme, authPadding)
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
			wantOut:  &testWireFrame{cmd: cmdAlert, sid: 0, data: []byte("anytls: unexpected ServerSettings from client")},
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
			wantOut:  &testWireFrame{cmd: cmdAlert, sid: 0, data: []byte("anytls: PSH frame with empty payload, streamId=1")},
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

func TestSessionRejectsInvalidFrameShapes(t *testing.T) {
	tests := []struct {
		name     string
		isClient bool
		frame    testWireFrame
		wantText string
	}{
		{name: "settings-stream-id", frame: testWireFrame{cmd: cmdSettings, sid: 1, data: []byte("v=2")}, wantText: "Settings stream ID must be zero"},
		{name: "empty-settings", frame: testWireFrame{cmd: cmdSettings}, wantText: "Settings body must not be empty"},
		{name: "heart-request-stream-id", frame: testWireFrame{cmd: cmdHeartRequest, sid: 1}, wantText: "heartbeat stream ID must be zero"},
		{name: "heart-request-body", frame: testWireFrame{cmd: cmdHeartRequest, data: []byte("body")}, wantText: "heartbeat body must be empty"},
		{name: "heart-response-body", isClient: true, frame: testWireFrame{cmd: cmdHeartResponse, data: []byte("body")}, wantText: "heartbeat body must be empty"},
		{name: "fin-stream-id", frame: testWireFrame{cmd: cmdFIN}, wantText: "FIN stream ID must not be zero"},
		{name: "fin-body", frame: testWireFrame{cmd: cmdFIN, sid: 1, data: []byte("body")}, wantText: "FIN body must be empty"},
		{name: "psh-stream-id", frame: testWireFrame{cmd: cmdPSH, data: []byte("body")}, wantText: "PSH stream ID must not be zero"},
		{name: "synack-stream-id", isClient: true, frame: testWireFrame{cmd: cmdSYNACK}, wantText: "SYNACK stream ID must not be zero"},
		{name: "alert-stream-id", isClient: true, frame: testWireFrame{cmd: cmdAlert, sid: 1}, wantText: "Alert stream ID must be zero"},
		{name: "padding-update-stream-id", isClient: true, frame: testWireFrame{cmd: cmdUpdatePaddingScheme, sid: 1, data: []byte("stop=1\n0=30-30")}, wantText: "UpdatePaddingScheme stream ID must be zero"},
		{name: "empty-padding-update", isClient: true, frame: testWireFrame{cmd: cmdUpdatePaddingScheme}, wantText: "empty padding update"},
		{name: "server-settings-stream-id", isClient: true, frame: testWireFrame{cmd: cmdServerSettings, sid: 1, data: []byte("v=2")}, wantText: "ServerSettings stream ID must be zero"},
		{name: "empty-server-settings", isClient: true, frame: testWireFrame{cmd: cmdServerSettings}, wantText: "ServerSettings body must not be empty"},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			s, output := newWireSession(marshalTestFrames(tt.frame), tt.isClient)
			err := s.readLoop(context.Background())
			if err == nil || !strings.Contains(err.Error(), tt.wantText) {
				t.Fatalf("readLoop error = %v, want text %q", err, tt.wantText)
			}
			frames := parseTestFrames(t, output.Bytes())
			if tt.isClient {
				if len(frames) != 0 {
					t.Fatalf("client emitted frames for invalid server frame: %+v", frames)
				}
				return
			}
			if len(frames) != 1 || frames[0].cmd != cmdAlert || frames[0].sid != 0 || !strings.Contains(string(frames[0].data), tt.wantText) {
				t.Fatalf("server response = %+v, want Alert containing %q", frames, tt.wantText)
			}
		})
	}
}

func TestSessionInvalidFrameConsumesDeclaredBody(t *testing.T) {
	s, _ := newWireSession(marshalTestFrames(
		testWireFrame{cmd: cmdHeartRequest, sid: 0, data: []byte("invalid body")},
		testWireFrame{cmd: cmdWaste, sid: 0, data: []byte("next frame")},
	), false)

	err := s.readLoop(context.Background())
	if err == nil || !strings.Contains(err.Error(), "heartbeat body must be empty") {
		t.Fatalf("readLoop error = %v, want invalid heartbeat body", err)
	}
	var header [7]byte
	if _, err := io.ReadFull(s.br, header[:]); err != nil {
		t.Fatal(err)
	}
	if header[0] != cmdWaste || binary.BigEndian.Uint32(header[1:5]) != 0 {
		t.Fatalf("next frame header = %x, want Waste with stream ID 0", header)
	}
	length := int(binary.BigEndian.Uint16(header[5:7]))
	body := make([]byte, length)
	if _, err := io.ReadFull(s.br, body); err != nil {
		t.Fatal(err)
	}
	if string(body) != "next frame" {
		t.Fatalf("next frame body = %q, want %q", body, "next frame")
	}
}

func TestSessionRejectsDuplicateServerSettings(t *testing.T) {
	s, _ := newWireSession(marshalTestFrames(
		testWireFrame{cmd: cmdServerSettings, sid: 0, data: []byte("v=2")},
		testWireFrame{cmd: cmdServerSettings, sid: 0, data: []byte("v=2")},
	), true)

	err := s.readLoop(context.Background())
	if err == nil || !strings.Contains(err.Error(), "duplicate ServerSettings") {
		t.Fatalf("readLoop error = %v, want duplicate ServerSettings", err)
	}
	if s.peerVersionValue() != 2 || !s.serverSettingsReceived {
		t.Fatalf("first ServerSettings was not applied: version=%d received=%v", s.peerVersionValue(), s.serverSettingsReceived)
	}
}

func TestSessionDiscardsPSHForClosedStream(t *testing.T) {
	for _, streamInMap := range []bool{false, true} {
		t.Run(fmt.Sprintf("stream_in_map_%t", streamInMap), func(t *testing.T) {
			input := marshalTestFrames(
				testWireFrame{cmd: cmdPSH, sid: 9, data: []byte("late payload")},
				testWireFrame{cmd: cmdHeartRequest, sid: 0},
			)
			s, output := newWireSession(input, false)
			s.handshakeDone = true
			if streamInMap {
				endpoint := newTestLinkEndpoint()
				defer endpoint.closeInput()
				defer endpoint.closeOutput()
				st := newStream(9, endpoint.link)
				st.close(io.ErrClosedPipe)
				s.streams[9] = st
			}

			if err := s.readLoop(context.Background()); !errors.Is(err, io.EOF) {
				t.Fatalf("readLoop error = %v, want EOF", err)
			}
			frames := parseTestFrames(t, output.Bytes())
			if len(frames) != 1 || frames[0].cmd != cmdHeartResponse || frames[0].sid != 0 {
				t.Fatalf("frames after late PSH = %+v, want heartbeat response", frames)
			}
		})
	}
}

func TestSessionSYNWithBodyClosesSession(t *testing.T) {
	s, output := newWireSession(marshalTestFrames(
		testWireFrame{cmd: cmdSYN, sid: 7, data: []byte("unexpected")},
	), false)
	s.handshakeDone = true
	err := s.readLoop(context.Background())
	if err == nil || !strings.Contains(err.Error(), "SYN body must be empty") {
		t.Fatalf("readLoop error = %v, want invalid SYN body", err)
	}
	frames := parseTestFrames(t, output.Bytes())
	if len(frames) != 1 || frames[0].cmd != cmdAlert || frames[0].sid != 0 || !strings.Contains(string(frames[0].data), "SYN body must be empty") {
		t.Fatalf("SYN body response = %+v, want Alert", frames)
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

func TestSessionRequiresNewMonotonicSYNStreamIDs(t *testing.T) {
	tests := []struct {
		name     string
		frames   []testWireFrame
		wantText string
	}{
		{
			name:     "zero",
			frames:   []testWireFrame{{cmd: cmdSYN, sid: 0}},
			wantText: "must not be zero",
		},
		{
			name:     "duplicate-active",
			frames:   []testWireFrame{{cmd: cmdSYN, sid: 7}, {cmd: cmdSYN, sid: 7}},
			wantText: "must increase",
		},
		{
			name:     "out-of-order",
			frames:   []testWireFrame{{cmd: cmdSYN, sid: 7}, {cmd: cmdSYN, sid: 6}},
			wantText: "must increase",
		},
		{
			name: "reuse-after-fin",
			frames: []testWireFrame{
				{cmd: cmdSYN, sid: 7},
				{cmd: cmdFIN, sid: 7},
				{cmd: cmdSYN, sid: 7},
			},
			wantText: "must increase",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			s, _ := newWireSession(marshalTestFrames(tt.frames...), false)
			s.handshakeDone = true
			err := s.readLoop(context.Background())
			if err == nil || !strings.Contains(err.Error(), tt.wantText) {
				t.Fatalf("readLoop error = %v, want text %q", err, tt.wantText)
			}
		})
	}
}

func TestSessionAcceptsIncreasingSYNStreamIDsWithGaps(t *testing.T) {
	s, _ := newWireSession(marshalTestFrames(
		testWireFrame{cmd: cmdSYN, sid: 1},
		testWireFrame{cmd: cmdSYN, sid: 3},
	), false)
	s.handshakeDone = true

	if err := s.readLoop(context.Background()); !errors.Is(err, io.EOF) {
		t.Fatalf("readLoop error = %v, want EOF", err)
	}
	if s.lastPeerSID != 3 || s.streams[1] == nil || s.streams[3] == nil {
		t.Fatalf("accepted stream state = last:%d streams:%v", s.lastPeerSID, s.streams)
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
