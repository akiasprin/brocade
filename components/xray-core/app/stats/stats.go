package stats

import (
	"context"
	"strings"
	"sync"
	"sync/atomic"
	"time"

	"github.com/xtls/xray-core/common"
	"github.com/xtls/xray-core/common/errors"
	"github.com/xtls/xray-core/common/siteusage"
	"github.com/xtls/xray-core/features/stats"
)

// Manager is an implementation of stats.Manager.
type Manager struct {
	access      sync.RWMutex
	counters    map[string]*Counter
	onlineMaps  map[string]*OnlineMap
	channels    map[string]*Channel
	running     bool
	journal     counterJournal
	siteJournal counterJournal
	siteLimited *Counter
	sitePending *Counter
	siteEnabled atomic.Bool
	siteStop    chan struct{}
	// Filesystem registration never holds access or the cold-cache mutex.
	siteRegistration sync.Mutex
	siteCacheMu      sync.Mutex
	sitePairs        sync.Map
	sitePairCount    int
	siteQueue        chan *siteCounterPair
	siteDone         chan struct{}
	siteWorkerOnce   sync.Once
	siteCloseOnce    sync.Once
	siteClosing      bool
	otherDomains     otherDomainCollector
	failedTargets    otherDomainCollector
	sourceTraffic    otherDomainCollector
	otherDomainStop  chan struct{}
}

// NewManager creates an instance of Statistics Manager.
func NewManager(ctx context.Context, config *Config) (*Manager, error) {
	m := &Manager{
		counters:   make(map[string]*Counter),
		onlineMaps: make(map[string]*OnlineMap),
		channels:   make(map[string]*Channel),
	}
	journal, err := openCounterJournal()
	if err != nil {
		return nil, err
	}
	if journal != nil {
		m.journal = journal
		for name, counter := range journal.Counters() {
			m.counters[name] = counter
		}
	}

	if err := m.initSiteUsage(); err != nil {
		// Site observations are ancillary: a damaged optional journal must
		// never prevent authentication, routing or required billing.
		errors.LogWarning(ctx, "site usage unavailable: ", err)
		m.siteEnabled.Store(false)
		m.siteJournal = nil
		for name := range m.counters {
			if strings.HasPrefix(name, "site>>>") {
				delete(m.counters, name)
			}
		}
	}
	m.initOtherDomains()
	m.sourceTraffic.bucketSeconds = 30
	m.sourceTraffic.update(sourceTrafficPolicy(), time.Now().Unix())
	return m, nil
}

func (m *Manager) initSiteUsage() error {
	var siteEpoch string
	var err error
	m.siteJournal, siteEpoch, err = openSiteJournal()
	if err != nil {
		return err
	}
	if m.siteJournal != nil {
		marker := new(Counter)
		marker.Set(1)
		m.counters["site>>>epoch:"+siteEpoch] = marker
		for name, counter := range m.siteJournal.Counters() {
			m.counters[name] = counter
		}
		// A persisted marker reports the file identity even when there are no
		// user connections yet, and survives normal process replacements.
		if m.siteLimited = m.counters["site>>>limited"]; m.siteLimited == nil {
			m.siteLimited, err = m.siteJournal.Register("site>>>limited")
			if err != nil {
				return err
			}
			m.counters["site>>>limited"] = m.siteLimited
		}
		version := m.counters["site>>>version"]
		if version == nil {
			version, err = m.siteJournal.Register("site>>>version")
			if err != nil {
				return err
			}
			version.Set(int64(siteusage.Version()))
			m.counters["site>>>version"] = version
		}
		if version.Value() == 0 {
			version.Set(int64(siteusage.Version()))
		}
		if version.Value() != int64(siteusage.Version()) {
			return errors.New("site journal catalog version mismatch; a new catalog requires an independent journal")
		}
		for _, name := range []string{"site>>>enabled", "site>>>activation", "site>>>pending"} {
			if m.counters[name] == nil {
				m.counters[name], err = m.siteJournal.Register(name)
				if err != nil {
					return err
				}
			}
		}
		m.sitePending = m.counters["site>>>pending"]
		enabled, policyErr := siteUsagePolicy()
		if policyErr != nil {
			// A broken control file disables only observations. Continue
			// watching so replacing it with a valid policy can recover.
			errors.LogWarning(context.Background(), "site usage policy unavailable; collection disabled")
			enabled = false
		}
		m.setSiteEnabled(enabled)
		m.siteQueue = make(chan *siteCounterPair, siteRegistrationBacklog)
		m.siteDone = make(chan struct{})
	}
	return nil
}

func (m *Manager) SiteStatsEnabled() bool { return m.siteEnabled.Load() }

// Type implements common.HasType.
func (*Manager) Type() interface{} {
	return stats.ManagerType()
}

// RegisterCounter implements stats.Manager.
func (m *Manager) RegisterCounter(name string) (stats.Counter, error) {
	if m.siteJournal != nil && strings.HasPrefix(name, "site>>>") {
		counter, err := m.registerSiteCounter(name, false)
		if err != nil {
			return nil, err
		}
		return m.siteCounter(name, counter), nil
	}
	m.access.Lock()
	defer m.access.Unlock()

	if _, found := m.counters[name]; found {
		return nil, errors.New("Counter ", name, " already registered.")
	}
	errors.LogDebug(context.Background(), "create new counter ", name)
	c := new(Counter)
	_, delegated := m.journal.(delegatedCounterJournal)
	if m.journal != nil && (delegated || journalCounterName(name)) {
		var err error
		c, err = m.journal.Register(name)
		if err != nil {
			return nil, err
		}
	}
	m.counters[name] = c
	return m.siteCounter(name, c), nil
}

// GetOrRegisterCounter implements stats.Manager.
func (m *Manager) GetOrRegisterCounter(name string) (stats.Counter, error) {
	return stats.GetOrRegisterCounter(m, name)
}

// UnregisterCounter implements stats.Manager.
func (m *Manager) UnregisterCounter(name string) error {
	m.access.Lock()
	defer m.access.Unlock()
	if _, delegated := m.journal.(delegatedCounterJournal); delegated {
		return errors.New("delegated accounting counters cannot be unregistered")
	}

	if _, found := m.counters[name]; found {
		errors.LogDebug(context.Background(), "remove counter ", name)
		delete(m.counters, name)
		if strings.HasPrefix(name, "site>>>") {
			m.siteCacheMu.Lock()
			m.sitePairs.Clear()
			m.sitePairCount = 0
			m.siteCacheMu.Unlock()
		}
	}
	return nil
}

// GetCounter implements stats.Manager.
func (m *Manager) GetCounter(name string) stats.Counter {
	m.access.RLock()
	defer m.access.RUnlock()

	if c, found := m.counters[name]; found {
		return m.siteCounter(name, c)
	}
	return nil
}

// VisitCounters calls visitor function on all managed counters.
func (m *Manager) VisitCounters(visitor func(string, stats.Counter) bool) {
	// Optional registration must not stall either forwarding or billing snapshots.
	// A busy journal is already marked pending; pick up shared labels next time.
	if m.siteRegistration.TryLock() {
		if refresh, ok := m.siteJournal.(interface{ refreshCounters() error }); ok {
			if err := refresh.refreshCounters(); err != nil {
				m.siteLimited.Set(1)
			}
		}
		m.access.Lock()
		if m.siteJournal != nil {
			for name, counter := range m.siteJournal.Counters() {
				m.counters[name] = counter
			}
		}
		m.siteRegistration.Unlock()
	} else {
		m.access.Lock()
	}
	defer m.access.Unlock()

	for name, c := range m.counters {
		if !visitor(name, c) {
			break
		}
	}
}

// RegisterOnlineMap implements stats.Manager.
func (m *Manager) RegisterOnlineMap(name string) (stats.OnlineMap, error) {
	m.access.Lock()
	defer m.access.Unlock()

	if _, found := m.onlineMaps[name]; found {
		return nil, errors.New("OnlineMap ", name, " already registered.")
	}
	errors.LogDebug(context.Background(), "create new OnlineMap ", name)
	om := NewOnlineMap()
	m.onlineMaps[name] = om
	return om, nil
}

// GetOrRegisterOnlineMap implements stats.Manager.
func (m *Manager) GetOrRegisterOnlineMap(name string) (stats.OnlineMap, error) {
	return stats.GetOrRegisterOnlineMap(m, name)
}

// UnregisterOnlineMap implements stats.Manager.
func (m *Manager) UnregisterOnlineMap(name string) error {
	m.access.Lock()
	defer m.access.Unlock()

	if _, found := m.onlineMaps[name]; found {
		errors.LogDebug(context.Background(), "remove OnlineMap ", name)
		delete(m.onlineMaps, name)
	}
	return nil
}

// GetOnlineMap implements stats.Manager.
func (m *Manager) GetOnlineMap(name string) stats.OnlineMap {
	m.access.RLock()
	defer m.access.RUnlock()

	if om, found := m.onlineMaps[name]; found {
		return om
	}
	return nil
}

// VisitOnlineMaps calls visitor function on all managed online maps.
// The visitor runs under a read lock; it must not call RegisterOnlineMap or UnregisterOnlineMap (would deadlock).
func (m *Manager) VisitOnlineMaps(visitor func(string, stats.OnlineMap) bool) {
	m.access.RLock()
	defer m.access.RUnlock()
	for name, om := range m.onlineMaps {
		if !visitor(name, om) {
			break
		}
	}
}

// RegisterChannel implements stats.Manager.
func (m *Manager) RegisterChannel(name string) (stats.Channel, error) {
	m.access.Lock()
	defer m.access.Unlock()

	if _, found := m.channels[name]; found {
		return nil, errors.New("Channel ", name, " already registered.")
	}
	errors.LogDebug(context.Background(), "create new channel ", name)
	c := NewChannel(&ChannelConfig{BufferSize: 64, Blocking: false})
	m.channels[name] = c
	if m.running {
		return c, c.Start()
	}
	return c, nil
}

// GetOrRegisterChannel implements stats.Manager.
func (m *Manager) GetOrRegisterChannel(name string) (stats.Channel, error) {
	return stats.GetOrRegisterChannel(m, name)
}

// UnregisterChannel implements stats.Manager.
func (m *Manager) UnregisterChannel(name string) error {
	m.access.Lock()
	defer m.access.Unlock()

	if c, found := m.channels[name]; found {
		errors.LogDebug(context.Background(), "remove channel ", name)
		delete(m.channels, name)
		return c.Close()
	}
	return nil
}

// GetChannel implements stats.Manager.
func (m *Manager) GetChannel(name string) stats.Channel {
	m.access.RLock()
	defer m.access.RUnlock()

	if c, found := m.channels[name]; found {
		return c
	}
	return nil
}

// GetAllOnlineUsers implements stats.Manager.
func (m *Manager) GetAllOnlineUsers() []string {
	m.access.RLock()
	defer m.access.RUnlock()

	usersOnline := make([]string, 0, len(m.onlineMaps))
	for user, om := range m.onlineMaps {
		if om.Count() > 0 {
			usersOnline = append(usersOnline, user)
		}
	}

	return usersOnline
}

// Start implements common.Runnable.
func (m *Manager) Start() error {
	m.access.Lock()
	defer m.access.Unlock()
	m.running = true
	m.otherDomains.closed.Store(false)
	m.failedTargets.closed.Store(false)
	m.sourceTraffic.closed.Store(false)
	if m.otherDomainStop == nil {
		m.otherDomainStop = make(chan struct{})
		go m.watchOtherDomains(m.otherDomainStop)
	}
	if m.siteJournal != nil && m.siteStop == nil {
		m.siteStop = make(chan struct{})
		go m.watchSiteUsage(m.siteStop)
		m.startSiteWorker()
	}
	errs := []error{}
	for _, channel := range m.channels {
		if err := channel.Start(); err != nil {
			errs = append(errs, err)
		}
	}
	if len(errs) != 0 {
		return errors.Combine(errs...)
	}
	return nil
}

// Close implement common.Closable.
func (m *Manager) Close() error {
	m.closeSiteUsage()
	m.access.Lock()
	defer m.access.Unlock()
	m.running = false
	m.otherDomains.closed.Store(true)
	m.failedTargets.closed.Store(true)
	m.sourceTraffic.closed.Store(true)
	if m.otherDomainStop != nil {
		close(m.otherDomainStop)
		m.otherDomainStop = nil
	}
	m.otherDomains.update(otherDomainPolicy{}, time.Now().Unix())
	m.failedTargets.update(otherDomainPolicy{}, time.Now().Unix())
	m.sourceTraffic.update(otherDomainPolicy{}, time.Now().Unix())
	if m.siteStop != nil {
		close(m.siteStop)
		m.siteStop = nil
	}
	for name := range m.onlineMaps {
		errors.LogDebug(context.Background(), "remove OnlineMap ", name)
		delete(m.onlineMaps, name)
	}
	errs := []error{}
	for name, channel := range m.channels {
		errors.LogDebug(context.Background(), "remove channel ", name)
		delete(m.channels, name)
		if err := channel.Close(); err != nil {
			errs = append(errs, err)
		}
	}
	if len(errs) != 0 {
		return errors.Combine(errs...)
	}
	return nil
}

func init() {
	common.Must(common.RegisterConfig((*Config)(nil), func(ctx context.Context, config interface{}) (interface{}, error) {
		return NewManager(ctx, config.(*Config))
	}))
}
