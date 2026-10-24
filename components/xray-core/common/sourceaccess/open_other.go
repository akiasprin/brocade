//go:build !unix

package sourceaccess

import "os"

func openPolicy(path string) (*os.File, error) { return os.Open(path) }
