package dispatcher

import (
	"testing"

	"github.com/xtls/xray-core/common/session"
)

func TestSetSniffingState(t *testing.T) {
	tests := []struct {
		name                     string
		originalDestinationWasIP bool
		domainRecovered          bool
		want                     string
	}{
		{name: "domain destination"},
		{
			name:                     "IP destination recovered",
			originalDestinationWasIP: true,
			domainRecovered:          true,
		},
		{
			name:                     "IP destination not recovered",
			originalDestinationWasIP: true,
			want:                     sniffingStateFailed,
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			content := &session.Content{
				Attributes: map[string]string{sniffingStateAttribute: sniffingStateFailed},
			}
			setSniffingState(content, test.originalDestinationWasIP, test.domainRecovered)
			if got := content.Attribute(sniffingStateAttribute); got != test.want {
				t.Fatalf("sniffing state = %q, want %q", got, test.want)
			}
		})
	}

	content := new(session.Content)
	setSniffingState(content, false, false)
	if content.Attributes != nil {
		t.Fatal("successful fast path allocated an attributes map")
	}
}
