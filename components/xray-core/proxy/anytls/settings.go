package anytls

import (
	"encoding/hex"
	"fmt"
	"strconv"
	"strings"
	"unicode/utf8"

	"github.com/xtls/xray-core/core"
)

type peerSettings struct {
	version    byte
	client     string
	paddingMD5 string
}

func clientMetadata() string {
	return "brocade-xray/" + core.Version() + "+" + core.Build()
}

func parseSettings(text string) (peerSettings, error) {
	if !utf8.ValidString(text) {
		return peerSettings{}, fmt.Errorf("anytls: settings are not valid UTF-8")
	}

	settings := peerSettings{version: 1}
	seen := make(map[string]struct{})
	for _, rawLine := range strings.Split(text, "\n") {
		line := strings.TrimSpace(rawLine)
		if line == "" {
			continue
		}
		parts := strings.SplitN(line, "=", 2)
		if len(parts) != 2 || strings.TrimSpace(parts[0]) == "" {
			return peerSettings{}, fmt.Errorf("anytls: malformed settings line")
		}
		key := strings.TrimSpace(parts[0])
		value := strings.TrimSpace(parts[1])
		if _, ok := seen[key]; ok {
			return peerSettings{}, fmt.Errorf("anytls: duplicate settings key %q", key)
		}
		seen[key] = struct{}{}

		switch key {
		case "v":
			version, err := strconv.ParseUint(value, 10, 8)
			if err == nil && version > 0 {
				settings.version = byte(version)
			}
		case "client":
			settings.client = value
		case "padding-md5":
			if len(value) != 32 {
				return peerSettings{}, fmt.Errorf("anytls: invalid padding-md5")
			}
			decoded, err := hex.DecodeString(value)
			if err != nil || len(decoded) != 16 {
				return peerSettings{}, fmt.Errorf("anytls: invalid padding-md5")
			}
			settings.paddingMD5 = strings.ToLower(value)
		}
	}
	return settings, nil
}
