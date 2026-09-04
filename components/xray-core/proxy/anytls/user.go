package anytls

import (
	"context"
	"crypto/sha256"

	"github.com/xtls/xray-core/common/errors"
	"github.com/xtls/xray-core/common/protocol"
)

// AddUser implements proxy.UserManager.AddUser().
func (s *Server) AddUser(ctx context.Context, u *protocol.MemoryUser) error {
	if u == nil || u.Account == nil {
		return errors.New("anytls: invalid user")
	}
	acc, ok := u.Account.(*MemoryAccount)
	if !ok {
		return errors.New("anytls: invalid account type")
	}

	sum := sha256.Sum256([]byte(acc.Password))

	s.userMu.Lock()
	var sessions []*session

	if prev, ok := s.usersByEmail[u.Email]; ok {
		sessions = s.detachUserSessionsLocked(prev)
		prevAcc, ok := prev.Account.(*MemoryAccount)
		if ok {
			prevSum := sha256.Sum256([]byte(prevAcc.Password))
			if prevSum != sum && s.users[prevSum] == prev {
				delete(s.users, prevSum)
			}
		}
	}
	s.users[sum] = u
	s.usersByEmail[u.Email] = u
	s.userMu.Unlock()
	closeUserSessions(sessions)
	return nil
}

// RemoveUser implements proxy.UserManager.RemoveUser().
func (s *Server) RemoveUser(ctx context.Context, email string) error {
	if email == "" {
		return errors.New("anytls: empty email")
	}

	s.userMu.Lock()
	var sessions []*session

	if user, ok := s.usersByEmail[email]; ok {
		delete(s.usersByEmail, email)
		sessions = s.detachUserSessionsLocked(user)
		acc, ok := user.Account.(*MemoryAccount)
		if ok {
			sum := sha256.Sum256([]byte(acc.Password))
			if s.users[sum] == user {
				delete(s.users, sum)
			}
		}
	}
	s.userMu.Unlock()
	closeUserSessions(sessions)
	return nil
}

func (s *Server) registerSession(sum [32]byte, sess *session) *protocol.MemoryUser {
	s.userMu.Lock()
	defer s.userMu.Unlock()

	user := s.users[sum]
	if user == nil {
		return nil
	}
	if s.activeSessions == nil {
		s.activeSessions = make(map[*protocol.MemoryUser]map[*session]struct{})
	}
	sessions := s.activeSessions[user]
	if sessions == nil {
		sessions = make(map[*session]struct{})
		s.activeSessions[user] = sessions
	}
	sessions[sess] = struct{}{}
	return user
}

func (s *Server) unregisterSession(user *protocol.MemoryUser, sess *session) {
	s.userMu.Lock()
	defer s.userMu.Unlock()

	sessions := s.activeSessions[user]
	delete(sessions, sess)
	if len(sessions) == 0 {
		delete(s.activeSessions, user)
	}
}

func (s *Server) detachUserSessionsLocked(user *protocol.MemoryUser) []*session {
	active := s.activeSessions[user]
	if len(active) == 0 {
		return nil
	}
	sessions := make([]*session, 0, len(active))
	for sess := range active {
		sessions = append(sessions, sess)
	}
	delete(s.activeSessions, user)
	return sessions
}

func closeUserSessions(sessions []*session) {
	err := errors.New("anytls: user revoked")
	for _, sess := range sessions {
		sess.close(err)
	}
}

// GetUser implements proxy.UserManager.GetUser().
func (s *Server) GetUser(ctx context.Context, email string) *protocol.MemoryUser {
	if email == "" {
		return nil
	}

	s.userMu.RLock()
	defer s.userMu.RUnlock()

	return s.usersByEmail[email]
}

// GetUsers implements proxy.UserManager.GetUsers().
func (s *Server) GetUsers(ctx context.Context) []*protocol.MemoryUser {
	s.userMu.RLock()
	defer s.userMu.RUnlock()

	users := make([]*protocol.MemoryUser, 0, len(s.users))
	for _, u := range s.users {
		users = append(users, u)
	}
	return users
}

// GetUsersCount implements proxy.UserManager.GetUsersCount().
func (s *Server) GetUsersCount(ctx context.Context) int64 {
	s.userMu.RLock()
	defer s.userMu.RUnlock()

	return int64(len(s.users))
}
