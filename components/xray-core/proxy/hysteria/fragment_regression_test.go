package hysteria

import (
	"bytes"
	"fmt"
	"io"
	"testing"
)

// Both server bootstrap and UDPReader parse messages out of a reusable buffer.
// Retained fragments must remain valid after the next datagram overwrites it.
func TestDefraggerPreservesReusedReadBuffer(t *testing.T) {
	for _, reverse := range []bool{false, true} {
		t.Run(fmt.Sprintf("reverse-%t", reverse), func(t *testing.T) {
			payload := regressionPayload(1452, 31)
			packets := regressionFragments(payload, 1197, 71)
			if reverse {
				for i, j := 0, len(packets)-1; i < j; i, j = i+1, j-1 {
					packets[i], packets[j] = packets[j], packets[i]
				}
			}
			var scratch [MaxUDPSize]byte
			defragger := &Defragger{}
			var complete *UDPMessage
			for _, packet := range packets {
				n := copy(scratch[:], packet)
				message, err := ParseUDPMessage(scratch[:n])
				if err != nil {
					t.Fatal(err)
				}
				complete = defragger.Feed(message)
			}
			if complete == nil || !bytes.Equal(complete.Data, payload) {
				t.Fatal("reassembled datagram changed after the shared read buffer was overwritten")
			}
			if complete.Addr != "127.0.0.1:443" {
				t.Fatalf("destination changed: %q", complete.Addr)
			}
		})
	}
}

func TestUDPReaderPreservesFragmentedDatagrams(t *testing.T) {
	for _, size := range []int{64, 1200, 1452} {
		t.Run(fmt.Sprintf("size-%d", size), func(t *testing.T) {
			var messages [][]byte
			for id := range 4 {
				messages = append(messages, regressionFragments(regressionPayload(size, byte(id)), 1197, uint16(id+1))...)
			}
			reader := &UDPReader{reader: &regressionDatagrams{messages: messages}, df: &Defragger{}}
			for id := range 4 {
				mb, err := reader.ReadMultiBuffer()
				if err != nil {
					t.Fatal(err)
				}
				if len(mb) != 1 {
					t.Fatalf("datagram boundary changed: %d buffers", len(mb))
				}
				got := bytes.Clone(mb[0].Bytes())
				mb[0].Release()
				if !bytes.Equal(got, regressionPayload(size, byte(id))) {
					t.Fatalf("datagram %d corrupted", id)
				}
			}
		})
	}
}

func TestParseUDPMessageFragmentOwnership(t *testing.T) {
	for _, fragmentCount := range []uint8{1, 2} {
		payload := regressionPayload(64, 5)
		message := &UDPMessage{Addr: "127.0.0.1:443", FragCount: fragmentCount, Data: payload}
		wire := make([]byte, message.Size())
		n := message.Serialize(wire)
		parsed, err := ParseUDPMessage(wire[:n])
		if err != nil {
			t.Fatal(err)
		}
		for i := range wire {
			wire[i] = 0
		}
		if fragmentCount > 1 && !bytes.Equal(parsed.Data, payload) {
			t.Fatal("retained fragment aliases caller storage")
		}
		if fragmentCount == 1 && parsed.Data[0] != 0 {
			t.Fatal("unfragmented fast path unexpectedly copied its payload")
		}
	}
}

func FuzzUDPFragmentReassembly(f *testing.F) {
	f.Add(regressionPayload(1200, 1), uint16(685), false)
	f.Add(regressionPayload(1452, 2), uint16(0), true)
	f.Fuzz(func(t *testing.T, payload []byte, mtuSeed uint16, reverse bool) {
		if len(payload) == 0 || len(payload) > MaxUDPSize {
			t.Skip()
		}
		packets := regressionFragments(payload, 512+int(mtuSeed)%686, 17)
		if reverse {
			for i, j := 0, len(packets)-1; i < j; i, j = i+1, j-1 {
				packets[i], packets[j] = packets[j], packets[i]
			}
		}
		var scratch [MaxUDPSize]byte
		df := &Defragger{}
		var complete *UDPMessage
		for _, packet := range packets {
			n := copy(scratch[:], packet)
			message, err := ParseUDPMessage(scratch[:n])
			if err != nil {
				t.Fatal(err)
			}
			complete = df.Feed(message)
		}
		if complete == nil || !bytes.Equal(complete.Data, payload) {
			t.Fatal("fragment reassembly changed the payload")
		}
	})
}

func regressionPayload(size int, seed byte) []byte {
	payload := make([]byte, size)
	for i := range payload {
		payload[i] = byte((i + int(seed)) % 251)
	}
	return payload
}

func regressionFragments(payload []byte, mtu int, id uint16) [][]byte {
	fragments := FragUDPMessage(&UDPMessage{PacketID: id, FragCount: 1, Addr: "127.0.0.1:443", Data: payload}, mtu)
	packets := make([][]byte, 0, len(fragments))
	for _, fragment := range fragments {
		wire := make([]byte, fragment.Size())
		n := fragment.Serialize(wire)
		packets = append(packets, wire[:n])
	}
	return packets
}

type regressionDatagrams struct{ messages [][]byte }

func (r *regressionDatagrams) Read(p []byte) (int, error) {
	if len(r.messages) == 0 {
		return 0, io.EOF
	}
	message := r.messages[0]
	r.messages = r.messages[1:]
	if len(message) > len(p) {
		return 0, io.ErrShortBuffer
	}
	return copy(p, message), nil
}
