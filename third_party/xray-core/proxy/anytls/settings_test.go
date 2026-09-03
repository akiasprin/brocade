package anytls

import (
	"encoding/hex"
	"strings"
	"testing"
)

func TestParseSettings(t *testing.T) {
	paddingMD5 := hex.EncodeToString(make([]byte, 16))
	settings, err := parseSettings("v=2\nclient=sing-anytls/0.0.13\npadding-md5=" + paddingMD5 + "\nunknown=value\n")
	if err != nil {
		t.Fatal(err)
	}
	if settings.version != 2 || settings.client != "sing-anytls/0.0.13" || settings.paddingMD5 != paddingMD5 {
		t.Fatalf("unexpected settings: %+v", settings)
	}

	tests := []string{
		"not-a-setting",
		"=value",
		"v=1\nv=2",
		"padding-md5=short",
		"padding-md5=" + strings.Repeat("z", 32),
		string([]byte{'c', 'l', 'i', 'e', 'n', 't', '=', 0xff}),
	}
	for _, raw := range tests {
		t.Run(raw, func(t *testing.T) {
			if _, err := parseSettings(raw); err == nil {
				t.Fatalf("parseSettings(%q) unexpectedly succeeded", raw)
			}
		})
	}
}

func TestParseSettingsDefaultsAndVersionHandling(t *testing.T) {
	settings, err := parseSettings("client=\n v=0\n")
	if err != nil {
		t.Fatal(err)
	}
	if settings.version != 1 || settings.client != "" {
		t.Fatalf("unexpected defaults: %+v", settings)
	}

	for _, version := range []string{"255", "256", "not-a-number"} {
		settings, err := parseSettings("v=" + version)
		if err != nil {
			t.Fatal(err)
		}
		if settings.version == 0 {
			t.Fatalf("version %q produced zero version", version)
		}
	}
}

func TestClientMetadataContainsBrocadeBuildIdentity(t *testing.T) {
	metadata := clientMetadata()
	if !strings.HasPrefix(metadata, "brocade-xray/26.4.25+") {
		t.Fatalf("metadata = %q, missing version/build prefix", metadata)
	}
}
