package config

import "testing"

func TestAdmissionQuotaCannotInflatePrewarmPool(t *testing.T) {
	before := cfg
	t.Cleanup(func() { cfg = before })
	cfg = &Config{HostConf: &HostConf{Quota: HostConfigQuota{MvmLimit: 3000}}}
	if got := GetPoolSizeForInit(1); got != 1 {
		t.Fatalf("prewarm count %d, want 1", got)
	}
	cfg.HostConf.Quota.MvmLimit = 2
	if got := GetPoolSizeForInit(8); got != 2 {
		t.Fatalf("prewarm count %d exceeds smaller quota", got)
	}
}
