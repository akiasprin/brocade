// Package core provides an entry point to use Xray core functionalities.
//
// Xray makes it possible to accept incoming network connections with certain
// protocol, process the data, and send them through another connection with
// the same or a difference protocol on demand.
//
// It may be configured to work with multiple protocols at the same time, and
// uses the internal router to tunnel through different inbound and outbound
// connections.
package core

import (
	"fmt"
	"runtime"
	"runtime/debug"

	"github.com/xtls/xray-core/common/serial"
)

var (
	Version_x byte = 26
	Version_y byte = 10
	Version_z byte = 21
)

var (
	build    = "Custom"
	codename = "Xray, Penetrates Everything."
	intro    = "A unified platform for anti-censorship."
)

const brocadeCapabilities = "user-traffic-limit-v1 machine-traffic-limit-v1 anytls-ktls-splice anytls-ktls-writev atomic-routing-reload-v1 config-listener-handoff-v1 control-listener-handoff-v1 detail-checkpoints-v2 forwarding-close-connections-v1 forwarding-target-policy-v1 generation-retirement-v2 listener-handoff-v1 other-domain-usage-v1 secure-dokodemo-splice sniffing-failed-targets-v1 tcp-handoff-udp-reset-v1 user-session-revocation-v1 user-site-usage-v1 source-access-v1 probe-connection-evidence-v2 probe-dial-errors-v1 reality-legacy-x25519-v1 reality-empty-ee-v1 reverse-drain-fence-v1"

func init() {
	// Manually injected
	if build != "Custom" {
		return
	}
	info, ok := debug.ReadBuildInfo()
	if !ok {
		return
	}
	var isDirty bool
	var foundBuild bool
	for _, setting := range info.Settings {
		switch setting.Key {
		case "vcs.revision":
			if len(setting.Value) < 7 {
				return
			}
			build = setting.Value[:7]
			foundBuild = true
		case "vcs.modified":
			isDirty = setting.Value == "true"
		}
	}
	if isDirty && foundBuild {
		build += "-dirty"
	}
}

// Version returns Xray's version as a string, in the form of "x.y.z" where x, y and z are numbers.
// ".z" part may be omitted in regular releases.
func Version() string {
	return fmt.Sprintf("%v.%v.%v", Version_x, Version_y, Version_z)
}

// Build returns the build identity shown in VersionStatement.
func Build() string {
	return build
}

// VersionStatement returns a list of strings representing the full version info.
func VersionStatement() []string {
	return []string{
		serial.Concat("Xray ", Version(), " (", codename, ") ", build, " (", runtime.Version(), " ", runtime.GOOS, "/", runtime.GOARCH, ")"),
		intro,
		"Brocade-Capabilities: " + brocadeCapabilities,
	}
}
