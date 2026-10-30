//go:build coverage
// +build coverage

package scenarios

import (
	"bytes"
	"fmt"
	"os"
	"os/exec"
)

func BuildXray() error {
	if os.Getenv("BROCADE_XRAY_BIN") != "" {
		return fmt.Errorf("coverage requires an instrumented build; unset BROCADE_XRAY_BIN")
	}
	genTestBinaryPath()
	if _, err := os.Stat(testBinaryPath); err == nil {
		return nil
	}

	cmd := exec.Command("go", "test", "-tags", "coverage coveragemain", "-coverpkg", "github.com/xtls/xray-core/...", "-c", "-o", testBinaryPath, GetSourcePath())
	return cmd.Run()
}

func RunXrayProtobuf(config []byte) *exec.Cmd {
	genTestBinaryPath()

	covDir := os.Getenv("XRAY_COV")
	if covDir == "" {
		panic("XRAY_COV must be set when running scenario coverage")
	}
	if err := os.MkdirAll(covDir, 0o755); err != nil {
		panic(err)
	}
	proc := exec.Command(testBinaryPath, "-test.run", "^TestRunMainForCoverage$", "run", "-config=stdin:", "-format=pb")
	proc.Env = append(os.Environ(), "GOCOVERDIR="+covDir)
	proc.Stdin = bytes.NewBuffer(config)
	proc.Stderr = os.Stderr
	proc.Stdout = os.Stdout

	return proc
}
