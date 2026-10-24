//go:build !unix

package machinelimit

import (
	"github.com/xtls/xray-core/common/platform/filesystem"
	"os"
)

func openControl(path string) (*os.File, error) { return filesystem.OpenControlFile(path) }
