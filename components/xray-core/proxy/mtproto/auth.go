package mtproto

import (
	"crypto/sha256"
	"io"
)

const headerSize = 64

type authentication struct {
	header        [headerSize]byte
	decodingKey   [32]byte
	encodingKey   [32]byte
	decodingNonce [16]byte
	encodingNonce [16]byte
}

func (a *authentication) dataCenterID() (int16, bool) {
	value := int16(a.header[60]) | int16(a.header[61])<<8
	if value == 0 {
		return 0, false
	}
	return value, true
}

func (a *authentication) connectionType() [4]byte {
	var value [4]byte
	copy(value[:], a.header[56:60])
	return value
}

func (a *authentication) applySecret(secret [16]byte) {
	decoding := make([]byte, 0, len(a.decodingKey)+len(secret))
	decoding = append(decoding, a.decodingKey[:]...)
	decoding = append(decoding, secret[:]...)
	a.decodingKey = sha256.Sum256(decoding)

	encoding := make([]byte, 0, len(a.encodingKey)+len(secret))
	encoding = append(encoding, a.encodingKey[:]...)
	encoding = append(encoding, secret[:]...)
	a.encodingKey = sha256.Sum256(encoding)
}

func readAuthentication(reader io.Reader) (*authentication, error) {
	auth := new(authentication)
	if _, err := io.ReadFull(reader, auth.header[:]); err != nil {
		return nil, err
	}
	copy(auth.decodingKey[:], auth.header[8:40])
	copy(auth.decodingNonce[:], auth.header[40:56])
	for index := range auth.encodingKey {
		auth.encodingKey[index] = auth.header[55-index]
	}
	for index := range auth.encodingNonce {
		auth.encodingNonce[index] = auth.header[23-index]
	}
	return auth, nil
}
