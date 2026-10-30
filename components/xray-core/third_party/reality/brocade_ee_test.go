package reality

import "testing"

func TestBrocadeServerNameAckFitsCover(t *testing.T) {
	for _, tt := range []struct {
		name            string
		length          int
		initially, want bool
	}{
		{"empty_cover", 28, true, false},
		{"cover_with_ack", 32, true, true},
		{"larger_cover", 37, true, true},
		{"no_record_mimic", 0, true, true},
		{"already_absent", 28, false, false},
	} {
		t.Run(tt.name, func(t *testing.T) {
			extensions := &encryptedExtensionsMsg{serverNameAck: tt.initially}
			if err := fitServerNameAckToRecordedRecord(extensions, tt.length, 16); err != nil {
				t.Fatal(err)
			}
			if extensions.serverNameAck != tt.want {
				t.Fatalf("ack=%v want=%v", extensions.serverNameAck, tt.want)
			}
			encoded, err := extensions.marshal()
			if err != nil {
				t.Fatal(err)
			}
			if tt.length != 0 && recordHeaderLen+len(encoded)+1+16 > tt.length {
				t.Fatal("record still exceeds cover")
			}
		})
	}
}
