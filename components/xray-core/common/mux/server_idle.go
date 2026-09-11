package mux

import (
	"sync"
	"time"
)

const serverIdleCheckInterval = 60 * time.Second

// This is a peer-silence fallback, NOT the client's business idle TTL. It
// covers the largest pool probe interval (60s), 20% jitter and probe timeout
// (10s). Healthy pool clients choose when to retire their idle connections.
// Checks run once a minute, so an idle orphan is reclaimed 2–3 minutes after
// its last received activity / transition to zero business sessions.
const serverPeerIdleGrace = 2 * maxWorkerPoolProbeInterval

type serverIdleGuard struct {
	mu           sync.Mutex
	peerProbes   bool
	lastReceived time.Time
	idleSince    time.Time
	lastCount    int
	wasActive    bool
	clock        func() time.Time // Immutable after construction; nil uses the real clock.
}

func (s *serverIdleGuard) nowLocked() time.Time {
	if s.clock != nil {
		return s.clock()
	}
	return time.Now()
}

// Only a parsed ordinary pool Ping opts into peer-aware idle retention.
// Later business frames are also peer activity; local writes, unsolicited
// Pongs and reverse-only control frames never renew this guard.
func (s *serverIdleGuard) received(ping bool) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.peerProbes = s.peerProbes || ping
	if s.peerProbes {
		s.lastReceived = s.nowLocked()
	}
}

func (s *serverIdleGuard) closeIfIdle(sessions *SessionManager, checkSize, checkCount int) bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.peerProbes {
		now := s.nowLocked()
		sessions.RLock()
		active, count := len(sessions.sessions) != 0, int(sessions.count)
		sessions.RUnlock()
		if !active && (s.wasActive || count != s.lastCount) {
			// Active clients need not Ping. Start transition grace when we
			// observe them become idle, including a short session entirely
			// between checks. Do not depend on an on-empty callback: closing
			// a session and running that callback are not one atomic operation.
			s.idleSince = now
		}
		s.wasActive, s.lastCount = active, count
		last := s.lastReceived
		if s.idleSince.After(last) {
			last = s.idleSince
		}
		if now.Before(last.Add(serverPeerIdleGrace)) {
			return false
		}
	}
	// Keep the ordinary session guards, including the allocation count, so a
	// business session is never killed or overlooked between two idle checks.
	// Holding mu through this decision linearizes it with received Pings.
	return sessions.CloseIfNoSessionAndIdle(checkSize, checkCount)
}
