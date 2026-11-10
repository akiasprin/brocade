package storage

import (
	"context"
	"encoding/json"
	"os"
	"os/exec"
	"path/filepath"
	"testing"
	"time"

	"github.com/tencentcloud/CubeSandbox/Cubelet/pkg/constants"
	"github.com/tencentcloud/CubeSandbox/Cubelet/pkg/cubecow"
	"github.com/tencentcloud/CubeSandbox/Cubelet/plugins/workflow"
	"github.com/tencentcloud/CubeSandbox/Cubelet/storage/cow"
	cb "github.com/tencentcloud/CubeSandbox/pkgs/proto/services/cubebox/v1"
	"golang.org/x/sys/unix"
)

func plainTestEngine(t *testing.T, root string) *cubecow.Engine {
	t.Helper()
	data, _ := json.Marshal(map[string]any{"log": map[string]any{}, "backend": map[string]any{"kind": "plain", "plain": map[string]any{"root_dir": root, "reserve_bytes": 0}}})
	engine, err := cubecow.InitWithoutLoggingFromJSON(string(data))
	if err != nil {
		t.Fatal(err)
	}
	return engine
}

func TestPlainInitializationAndOriginalDiskBinding(t *testing.T) {
	ctx := context.Background()
	root := filepath.Join(t.TempDir(), "pool")
	engine := plainTestEngine(t, root)
	defer engine.Close()
	manager := newLocalVolumeManager(engine, cow.BackendPlain)
	old := initPlainMediumDevice
	defer func() { initPlainMediumDevice = old }()
	formats := 0
	// Simulate mkfs changing the physical extent layout; commit restores the full reservation.
	initPlainMediumDevice = func(path string) error {
		formats++
		f, err := os.OpenFile(path, os.O_WRONLY, 0)
		if err != nil {
			return err
		}
		defer f.Close()
		if err = unix.Fallocate(int(f.Fd()), unix.FALLOC_FL_PUNCH_HOLE|unix.FALLOC_FL_KEEP_SIZE, 0, 1<<20); err != nil {
			return err
		}
		_, err = f.Write([]byte("formatted data"))
		return err
	}
	volume, err := manager.CreateDefaultMediumVolume(ctx, "test", "root", 1<<20)
	if err != nil {
		t.Fatal(err)
	}
	if err = checkPlainInitialization(volume.FilePath, volume.VolumeName, 1<<20); err != nil {
		t.Fatal(err)
	}
	if _, err = manager.CreateDefaultMediumVolume(ctx, "test", "root", 1<<20); err != nil || formats != 1 {
		t.Fatalf("repeated create formatted original: %v formats=%d", err, formats)
	}
	if _, err = manager.CreateDefaultMediumVolume(ctx, "test", "root", 2<<20); err == nil {
		t.Fatal("changed size was accepted")
	}
	if _, err = manager.CreateMemoryVolume(ctx, "snap", 1<<20); err == nil {
		t.Fatal("plain memory snapshot accepted")
	}
	if _, err = manager.CreateTemplateBuildRootfs(ctx, "snap", 1<<20); err == nil {
		t.Fatal("plain template build accepted")
	}
	original := volume.FilePath + ".original"
	if err = os.Rename(volume.FilePath, original); err != nil {
		t.Fatal(err)
	}
	f, err := os.OpenFile(volume.FilePath, os.O_CREATE|os.O_EXCL|os.O_RDWR, 0600)
	if err != nil {
		t.Fatal(err)
	}
	if err = unix.Fallocate(int(f.Fd()), 0, 0, 1<<20); err != nil {
		f.Close()
		t.Fatal(err)
	}
	f.Close()
	if err = checkPlainInitialization(volume.FilePath, volume.VolumeName, 1<<20); err == nil {
		t.Fatal("replacement disk accepted the original receipt")
	}
	if err = os.Remove(volume.FilePath); err != nil {
		t.Fatal(err)
	}
	if err = os.Rename(original, volume.FilePath); err != nil {
		t.Fatal(err)
	}
	if err = os.Remove(filepath.Join(filepath.Dir(volume.FilePath), ".initialized")); err != nil {
		t.Fatal(err)
	}
	if _, err = manager.CreateDefaultMediumVolume(ctx, "test", "root", 1<<20); err == nil || formats != 1 {
		t.Fatal("unconfirmed disk was reformatted")
	}
	if _, err = manager.GetVolumeInfo(ctx, volume.VolumeName); err == nil {
		t.Fatal("unconfirmed disk exposed to runtime")
	}
	if err = manager.DeleteByKind(ctx, volume.VolumeName, cow.KindVolume); err != nil {
		t.Fatal(err)
	}
}

func TestPlainSnapshotRequestRejectedBeforeStorageEffects(t *testing.T) {
	l := &local{config: &Config{StorageBackend: "cubecow", LocalBackend: cow.BackendPlain}}
	opts := &workflow.CreateContext{ReqInfo: &cb.RunCubeSandboxRequest{Backend: cow.BackendPlain, Annotations: map[string]string{constants.MasterAnnotationAppSnapshotTemplateID: "unsupported"}}}
	if err := l.Create(context.Background(), opts); err == nil {
		t.Fatal("plain snapshot restore accepted")
	}
}

func TestPlainConfigurationSelectsIndependentPool(t *testing.T) {
	cfg := &Config{DataPath: filepath.Join(t.TempDir(), "storage"), LocalBackend: cow.BackendPlain, PlainReserveBytes: 12345}
	if err := cfg.PrepareCowInlineConfig(); err != nil {
		t.Fatal(err)
	}
	payload, err := cfg.BuildCowInitJSON()
	if err != nil {
		t.Fatal(err)
	}
	var data map[string]any
	if err = json.Unmarshal(payload, &data); err != nil {
		t.Fatal(err)
	}
	backend := data["backend"].(map[string]any)
	if backend["kind"] != "plain" || backend["plain"].(map[string]any)["reserve_bytes"] != float64(12345) {
		t.Fatalf("wrong engine configuration: %s", payload)
	}
	local := &local{config: cfg, cowManager: &XfsCow{backend: cow.BackendPlain}}
	if _, err = local.storeForBackend(cow.BackendXFS); err == nil {
		t.Fatal("cross-pool binding accepted")
	}
	if _, err = local.storeForBackend("unknown"); err == nil {
		t.Fatal("unknown backend accepted")
	}
	if _, err = local.storeForBackend(cow.BackendPlain); err != nil {
		t.Fatal(err)
	}
	cfg.LocalBackend = "s3"
	if err = cfg.PrepareCowInlineConfig(); err == nil {
		t.Fatal("nonlocal pool accepted")
	}
}

// Run only in an explicitly selected disposable, privileged mount namespace.
func TestPlainRealFormatRecovery(t *testing.T) {
	if os.Getenv("BROCADE_RUN_PLAIN_FORMAT_TESTS") != "1" {
		t.Skip("requires isolated privileged mount namespace")
	}
	ctx := context.Background()
	root := filepath.Join(t.TempDir(), "pool")
	engine := plainTestEngine(t, root)
	manager := newLocalVolumeManager(engine, cow.BackendPlain)
	volume, err := manager.CreateDefaultMediumVolume(ctx, "real", "root", 64<<20)
	if err != nil {
		engine.Close()
		t.Fatal(err)
	}
	mounted := filepath.Join(t.TempDir(), "mnt")
	if err = os.Mkdir(mounted, 0700); err != nil {
		t.Fatal(err)
	}
	command := func(args ...string) {
		t.Helper()
		ctx, cancel := context.WithTimeout(ctx, 30*time.Second)
		defer cancel()
		if out, err := exec.CommandContext(ctx, args[0], args[1:]...).CombinedOutput(); err != nil {
			t.Fatalf("%v: %v %s", args, err, out)
		}
	}
	command("mount", volume.FilePath, mounted)
	if err = os.WriteFile(filepath.Join(mounted, "disk", "persistent"), []byte("original data"), 0600); err != nil {
		t.Fatal(err)
	}
	command("umount", mounted)
	info, err := os.Stat(volume.FilePath)
	if err != nil {
		t.Fatal(err)
	}
	engine.Close()
	engine = plainTestEngine(t, root)
	defer engine.Close()
	manager = newLocalVolumeManager(engine, cow.BackendPlain)
	reused, err := manager.CreateDefaultMediumVolume(ctx, "real", "root", 64<<20)
	if err != nil {
		t.Fatal(err)
	}
	other, err := os.Stat(reused.FilePath)
	if err != nil || !os.SameFile(info, other) {
		t.Fatal("original disk replaced")
	}
	command("mount", reused.FilePath, mounted)
	data, err := os.ReadFile(filepath.Join(mounted, "disk", "persistent"))
	if err != nil || string(data) != "original data" {
		t.Fatal("guest data lost")
	}
	command("umount", mounted)
	if err = checkPlainInitialization(reused.FilePath, reused.VolumeName, 64<<20); err != nil {
		t.Fatal(err)
	}
	if err = manager.DeleteByKind(ctx, reused.VolumeName, cow.KindVolume); err != nil {
		t.Fatal(err)
	}
	if _, err = os.Stat(reused.FilePath); !os.IsNotExist(err) {
		t.Fatal("physical deletion unconfirmed")
	}
}

func TestPlainConfigurationDoesNotReserveSpaceImplicitly(t *testing.T) {
	cfg := &Config{DataPath: filepath.Join(t.TempDir(), "storage"), LocalBackend: cow.BackendPlain}
	if err := cfg.PrepareCowInlineConfig(); err != nil {
		t.Fatal(err)
	}
	payload, err := cfg.BuildCowInitJSON()
	if err != nil {
		t.Fatal(err)
	}
	var data map[string]any
	if err := json.Unmarshal(payload, &data); err != nil {
		t.Fatal(err)
	}
	plain := data["backend"].(map[string]any)["plain"].(map[string]any)
	if plain["reserve_bytes"] != float64(0) {
		t.Fatalf("implicit physical reservation: %s", payload)
	}
}
