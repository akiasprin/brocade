package account

import (
	"github.com/google/uuid"
	"github.com/xtls/xray-core/common/net"
	"sync"

	"github.com/xtls/xray-core/common/errors"
	"github.com/xtls/xray-core/common/protocol"

	"google.golang.org/protobuf/proto"
)

func (a *Account) AsAccount() (protocol.Account, error) {
	var vr net.Port
	if id, err := uuid.Parse(a.Auth); err == nil {
		vr = net.PortFromBytes(id[6:8])
	}
	return &MemoryAccount{Auth: a.Auth, VR: vr}, nil
}

type MemoryAccount struct {
	Auth   string
	VR     net.Port
	source *protocol.MemoryUser
}

func (a *MemoryAccount) Equals(another protocol.Account) bool {
	if account, ok := another.(*MemoryAccount); ok {
		return a.Auth == account.Auth
	}
	return false
}

func (a *MemoryAccount) ToProto() proto.Message {
	return &Account{
		Auth: a.Auth,
	}
}

type Validator struct {
	emails map[string]struct{}
	users  map[string]*protocol.MemoryUser
	ids    map[uuid.UUID]*protocol.MemoryUser

	mutex    sync.Mutex
	sessions protocol.UserSessions
	managed  bool
}

func NewValidator() *Validator {
	return &Validator{
		emails: make(map[string]struct{}),
		users:  make(map[string]*protocol.MemoryUser),
		ids:    make(map[uuid.UUID]*protocol.MemoryUser),
	}
}

func (v *Validator) Add(u *protocol.MemoryUser) error {
	v.mutex.Lock()
	defer v.mutex.Unlock()
	u = &protocol.MemoryUser{Account: u.Account, Email: u.Email, Level: u.Level}
	if _, exists := v.users[u.Account.(*MemoryAccount).Auth]; exists {
		return errors.New("Hysteria authentication already exists")
	}

	id, uuidErr := uuid.Parse(u.Account.(*MemoryAccount).Auth)
	if uuidErr == nil {
		id[6], id[7] = 0, 0
		if _, exists := v.ids[id]; exists {
			return errors.New("Hysteria authentication identifier already exists")
		}
	}
	if u.Email != "" {
		if _, ok := v.emails[u.Email]; ok {
			return errors.New("User ", u.Email, " already exists.")
		}
		v.emails[u.Email] = struct{}{}
	}
	v.users[u.Account.(*MemoryAccount).Auth] = u
	if uuidErr == nil {
		v.ids[id] = u
	}
	v.managed = true

	return nil
}

// Managed mode remains enabled after the last dynamic user is removed. Falling back to a
// static transport password at that point would turn a complete revocation into a bypass.
func (v *Validator) IsManaged() bool {
	v.mutex.Lock()
	defer v.mutex.Unlock()
	return v.managed
}

func (v *Validator) Del(email string) error {
	if email == "" {
		return errors.New("Email must not be empty.")
	}

	v.mutex.Lock()

	if _, ok := v.emails[email]; !ok {
		v.mutex.Unlock()
		return errors.New("User ", email, " not found.")
	}
	delete(v.emails, email)
	var removed *protocol.MemoryUser
	for key, user := range v.users {
		if user.Email == email {
			delete(v.users, key)
			if id, err := uuid.Parse(key); err == nil {
				id[6], id[7] = 0, 0
				delete(v.ids, id)
			}
			removed = user
			break
		}
	}
	v.mutex.Unlock()
	v.sessions.CloseUser(removed)

	return nil
}

func (v *Validator) RegisterConnection(user *protocol.MemoryUser, close func()) (func(), bool) {
	account := user.Account.(*MemoryAccount)
	canonical := user
	if account.source != nil {
		canonical = account.source
	}
	return v.sessions.Register(canonical, func() bool {
		v.mutex.Lock()
		defer v.mutex.Unlock()
		return v.findAuthLocked(account.Auth) == canonical
	}, close)
}

func (v *Validator) findAuthLocked(auth string) *protocol.MemoryUser {
	if user := v.users[auth]; user != nil {
		return user
	}
	id, err := uuid.Parse(auth)
	if err != nil {
		return nil
	}
	id[6], id[7] = 0, 0
	return v.ids[id]
}
func (v *Validator) Get(auth string) *protocol.MemoryUser {
	v.mutex.Lock()
	defer v.mutex.Unlock()
	user := v.findAuthLocked(auth)
	if user == nil || user.Account.(*MemoryAccount).Auth == auth {
		return user
	}
	id, err := uuid.Parse(auth)
	if err != nil {
		return user
	}
	return &protocol.MemoryUser{Email: user.Email, Level: user.Level, Account: &MemoryAccount{Auth: auth, VR: net.PortFromBytes(id[6:8]), source: user}}
}
func (v *Validator) GetByID(id uuid.UUID) *protocol.MemoryUser {
	v.mutex.Lock()
	defer v.mutex.Unlock()
	return v.findAuthLocked(id.String())
}
func (v *Validator) DelByEmail(email string) error { return v.Del(email) }
func (v *Validator) NotEmpty() bool                { return v.GetCount() > 0 }

func (v *Validator) GetByEmail(email string) *protocol.MemoryUser {
	if email == "" {
		return nil
	}

	v.mutex.Lock()
	defer v.mutex.Unlock()

	if _, ok := v.emails[email]; ok {
		for _, user := range v.users {
			if user.Email == email {
				return user
			}
		}
	}

	return nil
}

func (v *Validator) GetAll() []*protocol.MemoryUser {
	v.mutex.Lock()
	defer v.mutex.Unlock()

	var users = make([]*protocol.MemoryUser, 0, len(v.users))
	for _, user := range v.users {
		users = append(users, user)
	}

	return users
}

func (v *Validator) GetCount() int64 {
	v.mutex.Lock()
	defer v.mutex.Unlock()

	return int64(len(v.users))
}
