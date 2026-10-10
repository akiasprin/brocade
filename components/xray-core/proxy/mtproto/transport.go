package mtproto

import (
	"bufio"
	"crypto/aes"
	"crypto/cipher"
	"crypto/rand"
	"encoding/binary"
	"fmt"
	"io"
	"net"
)

const (
	rpcFlagUnencrypted = uint32(2)
	rpcFlagMedium      = uint32(0x20000000)
	rpcFlagCompact     = uint32(0x40000000)
	rpcFlagQuickAck    = uint32(0x80000000)
	rpcFlagExtMode2    = uint32(0x00020000)
	maxMTProtoPacket   = 4 << 20
)

type clientTransport struct {
	typeTag [4]byte
	reader  *bufio.Reader
	writer  io.Writer
}

func newClientTransport(conn io.ReadWriter, auth *authentication) *clientTransport {
	request := requestPayloadStream(auth.decodingKey, auth.decodingNonce)
	response := newCTR(auth.encodingKey, auth.encodingNonce)
	return &clientTransport{
		typeTag: auth.connectionType(),
		reader:  bufio.NewReader(&cipher.StreamReader{S: request, R: conn}),
		writer:  &cipher.StreamWriter{S: response, W: conn},
	}
}

func newCTR(key [32]byte, nonce [16]byte) cipher.Stream {
	block, err := aes.NewCipher(key[:])
	if err != nil {
		panic(err)
	}
	return cipher.NewCTR(block, nonce[:])
}

func (t *clientTransport) readPacket() ([]byte, uint32, error) {
	switch t.typeTag {
	case [4]byte{0xef, 0xef, 0xef, 0xef}:
		return t.readAbridged()
	case [4]byte{0xee, 0xee, 0xee, 0xee}:
		return t.readIntermediate(false)
	case [4]byte{0xdd, 0xdd, 0xdd, 0xdd}:
		return t.readIntermediate(true)
	default:
		return nil, 0, fmt.Errorf("mtproto: unsupported client transport")
	}
}

func (t *clientTransport) readAbridged() ([]byte, uint32, error) {
	first, err := t.reader.ReadByte()
	if err != nil {
		return nil, 0, err
	}
	quick := first&0x80 != 0
	first &^= 0x80
	words := uint32(first)
	if first == 0x7f {
		var encoded [3]byte
		if _, err := io.ReadFull(t.reader, encoded[:]); err != nil {
			return nil, 0, err
		}
		words = uint32(encoded[0]) | uint32(encoded[1])<<8 | uint32(encoded[2])<<16
		if words < 0x7f {
			return nil, 0, fmt.Errorf("mtproto: overlong abridged packet length")
		}
	}
	length := words * 4
	if length < 4 || length > maxMTProtoPacket {
		return nil, 0, fmt.Errorf("mtproto: invalid abridged packet length %d", length)
	}
	payload := make([]byte, length)
	if _, err := io.ReadFull(t.reader, payload); err != nil {
		return nil, 0, err
	}
	flags := rpcFlagCompact | rpcFlagExtMode2
	if quick {
		flags |= rpcFlagQuickAck
	}
	return payload, flags, nil
}

func (t *clientTransport) readIntermediate(padded bool) ([]byte, uint32, error) {
	var encoded [4]byte
	if _, err := io.ReadFull(t.reader, encoded[:]); err != nil {
		return nil, 0, err
	}
	length := binary.LittleEndian.Uint32(encoded[:])
	quick := length&rpcFlagQuickAck != 0
	length &^= rpcFlagQuickAck
	if length < 4 || length > maxMTProtoPacket || (!padded && length%4 != 0) {
		return nil, 0, fmt.Errorf("mtproto: invalid intermediate packet length %d", length)
	}
	packet := make([]byte, length)
	if _, err := io.ReadFull(t.reader, packet); err != nil {
		return nil, 0, err
	}
	if padded {
		packet = packet[:len(packet)&^3]
		if len(packet) < 4 {
			return nil, 0, fmt.Errorf("mtproto: padded packet contains no aligned payload")
		}
	}
	flags := rpcFlagMedium | rpcFlagExtMode2
	if quick {
		flags |= rpcFlagQuickAck
	}
	return packet, flags, nil
}

func (t *clientTransport) writePacket(payload []byte) error {
	if len(payload) < 4 || len(payload)%4 != 0 {
		return fmt.Errorf("mtproto: invalid response payload length %d", len(payload))
	}
	var frame []byte
	switch t.typeTag {
	case [4]byte{0xef, 0xef, 0xef, 0xef}:
		words := len(payload) / 4
		if words < 0x7f {
			frame = append(frame, byte(words))
		} else if words <= 0xffffff {
			frame = append(frame, 0x7f, byte(words), byte(words>>8), byte(words>>16))
		} else {
			return fmt.Errorf("mtproto: response is too large for abridged transport")
		}
	case [4]byte{0xee, 0xee, 0xee, 0xee}:
		frame = binary.LittleEndian.AppendUint32(frame, uint32(len(payload)))
	case [4]byte{0xdd, 0xdd, 0xdd, 0xdd}:
		var random [1]byte
		if _, err := rand.Read(random[:]); err != nil {
			return fmt.Errorf("mtproto: generate response padding: %w", err)
		}
		padding := int(random[0] & 3)
		frame = binary.LittleEndian.AppendUint32(frame, uint32(len(payload)+padding))
		frame = append(frame, payload...)
		if padding != 0 {
			pad := make([]byte, padding)
			if _, err := rand.Read(pad); err != nil {
				return fmt.Errorf("mtproto: generate response padding: %w", err)
			}
			frame = append(frame, pad...)
		}
		return writeAll(t.writer, frame)
	default:
		return fmt.Errorf("mtproto: unsupported client transport")
	}
	frame = append(frame, payload...)
	return writeAll(t.writer, frame)
}

func (t *clientTransport) writeQuickAck(token uint32) error {
	var frame []byte
	switch t.typeTag {
	case [4]byte{0xef, 0xef, 0xef, 0xef}:
		frame = binary.BigEndian.AppendUint32(frame, token)
	case [4]byte{0xee, 0xee, 0xee, 0xee}:
		frame = binary.LittleEndian.AppendUint32(frame, token)
	case [4]byte{0xdd, 0xdd, 0xdd, 0xdd}:
		frame = binary.LittleEndian.AppendUint32(frame, 8)
		frame = binary.LittleEndian.AppendUint32(frame, ^uint32(0))
		frame = binary.LittleEndian.AppendUint32(frame, token)
	default:
		return fmt.Errorf("mtproto: unsupported client transport")
	}
	return writeAll(t.writer, frame)
}

func validateMTProtoRequest(payload []byte) (uint32, error) {
	if len(payload) < 8 || len(payload)%4 != 0 {
		return 0, fmt.Errorf("mtproto: malformed Telegram packet")
	}
	if binary.LittleEndian.Uint64(payload[:8]) != 0 {
		if len(payload) < 56 {
			return 0, fmt.Errorf("mtproto: encrypted Telegram packet is too short")
		}
		return 0, nil
	}
	if len(payload) < 28 {
		return 0, fmt.Errorf("mtproto: unencrypted Telegram packet is too short")
	}
	innerLength := int(binary.LittleEndian.Uint32(payload[16:20]))
	if innerLength < 20 || innerLength+20 > len(payload) {
		return 0, fmt.Errorf("mtproto: invalid unencrypted Telegram packet length")
	}
	function := binary.LittleEndian.Uint32(payload[20:24])
	switch function {
	case 0x60469778, 0xbe7e8ef1, 0xd712e4be, 0xf5045f1f:
		return rpcFlagUnencrypted, nil
	default:
		return 0, fmt.Errorf("mtproto: unsupported unencrypted Telegram handshake method")
	}
}

func proxyRequest(connectionID int64, flags uint32, remote, local net.Addr, payload []byte) ([]byte, error) {
	request := make([]byte, 0, 56+len(payload))
	request = binary.LittleEndian.AppendUint32(request, rpcProxyRequest)
	request = binary.LittleEndian.AppendUint32(request, flags)
	request = binary.LittleEndian.AppendUint64(request, uint64(connectionID))
	var err error
	request, err = appendRPCAddress(request, remote)
	if err != nil {
		return nil, err
	}
	request, err = appendRPCAddress(request, local)
	if err != nil {
		return nil, err
	}
	request = append(request, payload...)
	return request, nil
}

func appendRPCAddress(destination []byte, address net.Addr) ([]byte, error) {
	tcp, ok := address.(*net.TCPAddr)
	if !ok {
		return nil, fmt.Errorf("mtproto: client address is not TCP")
	}
	if ipv4 := tcp.IP.To4(); ipv4 != nil {
		destination = append(destination, make([]byte, 10)...)
		destination = append(destination, 0xff, 0xff)
		destination = append(destination, ipv4...)
	} else if ipv6 := tcp.IP.To16(); ipv6 != nil {
		destination = append(destination, ipv6...)
	} else {
		return nil, fmt.Errorf("mtproto: client address has no IP")
	}
	destination = binary.LittleEndian.AppendUint32(destination, uint32(tcp.Port))
	return destination, nil
}
