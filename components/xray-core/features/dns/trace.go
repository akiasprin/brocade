package dns

// LookupTrace is opt-in diagnostic evidence. Normal lookups do not allocate it.
// Tags identify configured policies, never resolver addresses or returned IPs.
type LookupTrace struct {
	Version  int               `json:"version"`
	Domain   string            `json:"domain,omitempty"`
	Strategy string            `json:"strategy,omitempty"`
	Complete bool              `json:"complete"`
	Steps    []LookupTraceStep `json:"steps"`
}

type LookupTraceStep struct {
	Tag     string              `json:"tag"`
	Scoped  bool                `json:"scoped"`
	Outcome string              `json:"outcome"`
	Other   bool                `json:"other"`
	Winner  uint32              `json:"winner,omitempty"`
	Members []LookupTraceMember `json:"members,omitempty"`
}

// Member indices are one-based within a configured logical DNS group. The
// immutable configuration fingerprint binds them to actual resolver addresses.
type LookupTraceMember struct {
	Index     uint32 `json:"index"`
	Outcome   string `json:"outcome"`
	Other     bool   `json:"other"`
	Attempts  uint32 `json:"attempts"`
	ElapsedMs uint32 `json:"elapsed_ms"`
	RCode     uint16 `json:"rcode,omitempty"`
	Cache     string `json:"cache,omitempty"`
}
