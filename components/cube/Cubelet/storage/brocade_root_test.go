package storage

import (
	"os"
	"path/filepath"
	"github.com/tencentcloud/CubeSandbox/Cubelet/plugins/workflow"
	"github.com/tencentcloud/CubeSandbox/pkgs/proto/services/cubebox/v1"
	"testing"
)

func TestBrocadeRootRetentionRequiresExplicitDataDeletion(t *testing.T) {
	info := &StorageInfo{BrocadeProtectedRoot: true}
	for _, opts := range []*workflow.DestroyContext{nil, {}, {DestroyInfo: &cubebox.DestroyCubeSandboxRequest{}}, {DestroyInfo: &cubebox.DestroyCubeSandboxRequest{Annotations: map[string]string{"brocade.cvm.delete_root": "false"}}}} {
		if !keepBrocadeRoot(info, opts) {
			t.Fatal("ordinary cleanup can delete protected root")
		}
	}
	deletion := &workflow.DestroyContext{DestroyInfo: &cubebox.DestroyCubeSandboxRequest{Annotations: map[string]string{"brocade.cvm.delete_root": "true"}}}
	if keepBrocadeRoot(info, deletion) {
		t.Fatal("explicit data deletion cannot clean root")
	}
	if keepBrocadeRoot(&StorageInfo{}, nil) {
		t.Fatal("unrelated Cube storage changed retention")
	}
}

func TestBrocadeRootManifestSurvivesLossOfTemporaryDatabase(t *testing.T) {
	l:=&local{config:&Config{DataPath:t.TempDir()}}
	info:=&StorageInfo{SandboxID:"aos-owned",BrocadeProtectedRoot:true,Volumes:map[string]*BackendFileInfo{"root":{Name:"root",VolumeName:"sb-aos-owned-root",Kind:"volume",FilePath:"/persistent/original"}}}
	if err:=l.persistBrocadeRoot(info);err!=nil{t.Fatal(err)}
	// A fresh storage process has no in-memory or tmpfs database to consult.
	fresh:=&local{config:&Config{DataPath:l.config.DataPath}}
	read,err:=fresh.readBrocadeRoot("aos-owned");if err!=nil || read.Volumes["root"].FilePath!="/persistent/original"{t.Fatalf("root binding lost: %v",err)}
	path:=filepath.Join(l.brocadeRootDir(),"aos-owned.json")
	if err:=os.Chmod(path,0666);err!=nil{t.Fatal(err)}
	if _,err:=fresh.readBrocadeRoot("aos-owned");err==nil{t.Fatal("untrusted manifest accepted")}
	info.Volumes["root"].VolumeName="sb-another-owner-root"
	if err:=l.persistBrocadeRoot(info);err==nil{t.Fatal("another instance root was accepted")}
}
