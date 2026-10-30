package vless

import (
	"strings"
	"sync"

	"github.com/xtls/xray-core/common/errors"
	"github.com/xtls/xray-core/common/protocol"
	"github.com/xtls/xray-core/common/uuid"
)

type Validator interface {
	Get(id uuid.UUID) *protocol.MemoryUser
	Add(u *protocol.MemoryUser) error
	Del(email string) error
	GetByEmail(email string) *protocol.MemoryUser
	GetAll() []*protocol.MemoryUser
	GetCount() int64
	RegisterConnection(*protocol.MemoryUser, func()) (func(), bool)
}

func ProcessUUID(id [16]byte) [16]byte {
	id[6] = 0
	id[7] = 0
	return id
}

// MemoryValidator stores valid VLESS users.
type MemoryValidator struct {
	// Considering email's usage here, map + sync.Mutex/RWMutex may have better performance.
	email    sync.Map
	users    sync.Map
	mutation sync.Mutex
	sessions protocol.UserSessions
}

// Add a VLESS user, Email must be empty or unique.
func (v *MemoryValidator) Add(u *protocol.MemoryUser) error {
	v.mutation.Lock()
	defer v.mutation.Unlock()
	// Identity belongs to this authorization, even when a caller reuses its input on regrant.
	u = &protocol.MemoryUser{Account: u.Account, Email: u.Email, Level: u.Level}
	id := ProcessUUID(u.Account.(*MemoryAccount).ID.UUID())
	if _, exists := v.users.Load(id); exists {
		return errors.New("User ID already exists")
	}
	if u.Email != "" {
		_, loaded := v.email.LoadOrStore(strings.ToLower(u.Email), u)
		if loaded {
			return errors.New("User ", u.Email, " already exists.")
		}
	}
	v.users.Store(id, u)
	return nil
}

// Del a VLESS user with a non-empty Email.
func (v *MemoryValidator) Del(e string) error {
	if e == "" {
		return errors.New("Email must not be empty.")
	}
	le := strings.ToLower(e)
	v.mutation.Lock()
	u, _ := v.email.Load(le)
	if u == nil {
		v.mutation.Unlock()
		return errors.New("User ", e, " not found.")
	}
	v.email.Delete(le)
	v.users.Delete(ProcessUUID(u.(*protocol.MemoryUser).Account.(*MemoryAccount).ID.UUID()))
	v.mutation.Unlock()
	v.sessions.CloseUser(u.(*protocol.MemoryUser))
	return nil
}

func (v *MemoryValidator) RegisterConnection(user *protocol.MemoryUser, close func()) (func(), bool) {
	return v.sessions.Register(user, func() bool {
		return v.Get(user.Account.(*MemoryAccount).ID.UUID()) == user
	}, close)
}

// Get a VLESS user with UUID, nil if user doesn't exist.
func (v *MemoryValidator) Get(id uuid.UUID) *protocol.MemoryUser {
	u, _ := v.users.Load(ProcessUUID(id))
	if u != nil {
		return u.(*protocol.MemoryUser)
	}
	return nil
}

// Get a VLESS user with email, nil if user doesn't exist.
func (v *MemoryValidator) GetByEmail(email string) *protocol.MemoryUser {
	email = strings.ToLower(email)
	u, _ := v.email.Load(email)
	if u != nil {
		return u.(*protocol.MemoryUser)
	}
	return nil
}

// Get all users
func (v *MemoryValidator) GetAll() []*protocol.MemoryUser {
	u := make([]*protocol.MemoryUser, 0, 100)
	v.email.Range(func(key, value interface{}) bool {
		u = append(u, value.(*protocol.MemoryUser))
		return true
	})
	return u
}

// Get users count
func (v *MemoryValidator) GetCount() int64 {
	var c int64 = 0
	v.email.Range(func(key, value interface{}) bool {
		c++
		return true
	})
	return c
}
