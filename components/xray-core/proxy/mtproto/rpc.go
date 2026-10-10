package mtproto

import (
	"bytes"
	"context"
	"crypto/aes"
	"crypto/cipher"
	"crypto/md5"
	"crypto/rand"
	"crypto/sha1"
	"encoding/binary"
	"fmt"
	"hash/crc32"
	"io"
	"net"
	"os"
	"sync"
	"time"

	xnet "github.com/xtls/xray-core/common/net"
	"github.com/xtls/xray-core/transport/internet"
)

const (
	rpcNonce          = uint32(0x7acb87aa)
	rpcHandshake      = uint32(0x7682eef5)
	rpcPing           = uint32(0x5730a2df)
	rpcPong           = uint32(0x8430eaa7)
	rpcProxyRequest   = uint32(0x36cef1ee)
	rpcProxyAnswer    = uint32(0x4403da0d)
	rpcCloseExternal  = uint32(0x5eb634a2)
	rpcSimpleAck      = uint32(0x3bac409b)
	rpcCryptoAES      = uint32(1)
	rpcFrameOverhead  = 12
	maxRPCPayloadSize = 8 << 20
)

type processID struct {
	ip    [4]byte
	port  int16
	pid   uint16
	utime int32
}

type middleRPC struct {
	conn net.Conn

	writeMu sync.Mutex
	encode  cipher.BlockMode
	decode  cipher.BlockMode
	plain   []byte
	outSeq  int32
	inSeq   int32

	connectionID int64
}

func dialMiddleRPC(ctx context.Context, target proxyTarget, secret []byte) (*middleRPC, error) {
	destination := xnet.TCPDestination(xnet.ParseAddress(target.host), xnet.Port(target.port))
	conn, err := internet.DialSystem(ctx, destination, nil)
	if err != nil {
		return nil, fmt.Errorf("mtproto: dial official middle proxy: %w", err)
	}
	if deadline, ok := ctx.Deadline(); ok {
		if err := conn.SetDeadline(deadline); err != nil {
			_ = conn.Close()
			return nil, fmt.Errorf("mtproto: set middle proxy handshake deadline: %w", err)
		}
	}
	rpc := &middleRPC{conn: conn, outSeq: -2, inSeq: -2}
	if err := rpc.handshake(secret); err != nil {
		_ = conn.Close()
		return nil, err
	}
	if err := conn.SetDeadline(time.Time{}); err != nil {
		_ = conn.Close()
		return nil, fmt.Errorf("mtproto: clear middle proxy handshake deadline: %w", err)
	}
	var id [8]byte
	if _, err := rand.Read(id[:]); err != nil {
		_ = conn.Close()
		return nil, fmt.Errorf("mtproto: generate middle proxy connection id: %w", err)
	}
	rpc.connectionID = int64(binary.LittleEndian.Uint64(id[:]))
	if rpc.connectionID == 0 {
		rpc.connectionID = 1
	}
	return rpc, nil
}

func (r *middleRPC) Close() error {
	return r.conn.Close()
}

func (r *middleRPC) handshake(secret []byte) error {
	if len(secret) < 32 || len(secret) > 256 {
		return fmt.Errorf("mtproto: invalid official middle proxy secret length")
	}
	var clientNonce [16]byte
	if _, err := rand.Read(clientNonce[:]); err != nil {
		return fmt.Errorf("mtproto: generate middle proxy nonce: %w", err)
	}
	timestamp := int32(time.Now().Unix())
	noncePayload := make([]byte, 32)
	binary.LittleEndian.PutUint32(noncePayload[0:4], rpcNonce)
	copy(noncePayload[4:8], secret[:4])
	binary.LittleEndian.PutUint32(noncePayload[8:12], rpcCryptoAES)
	binary.LittleEndian.PutUint32(noncePayload[12:16], uint32(timestamp))
	copy(noncePayload[16:32], clientNonce[:])
	if err := writeAll(r.conn, rpcFrame(r.outSeq, noncePayload)); err != nil {
		return fmt.Errorf("mtproto: send middle proxy nonce: %w", err)
	}
	r.outSeq++

	sequence, response, err := readPlainRPCFrame(r.conn)
	if err != nil {
		return fmt.Errorf("mtproto: read middle proxy nonce: %w", err)
	}
	if sequence != r.inSeq || len(response) != 32 || binary.LittleEndian.Uint32(response[:4]) != rpcNonce {
		return fmt.Errorf("mtproto: invalid middle proxy nonce response")
	}
	r.inSeq++
	if !bytes.Equal(response[4:8], secret[:4]) || binary.LittleEndian.Uint32(response[8:12]) != rpcCryptoAES {
		return fmt.Errorf("mtproto: middle proxy rejected official secret")
	}
	serverTimestamp := int32(binary.LittleEndian.Uint32(response[12:16]))
	if delta := serverTimestamp - timestamp; delta < -30 || delta > 30 {
		return fmt.Errorf("mtproto: middle proxy clock differs by more than 30 seconds")
	}
	var serverNonce [16]byte
	copy(serverNonce[:], response[16:32])

	local, ok := r.conn.LocalAddr().(*net.TCPAddr)
	if !ok {
		return fmt.Errorf("mtproto: middle proxy local address is not TCP")
	}
	remote, ok := r.conn.RemoteAddr().(*net.TCPAddr)
	if !ok {
		return fmt.Errorf("mtproto: middle proxy remote address is not TCP")
	}
	writeKey, writeIV, readKey, readIV, err := middleRPCKeys(serverNonce, clientNonce, timestamp, remote, local, secret)
	if err != nil {
		return err
	}
	writeBlock, err := aes.NewCipher(writeKey)
	if err != nil {
		return fmt.Errorf("mtproto: create middle proxy write cipher: %w", err)
	}
	readBlock, err := aes.NewCipher(readKey)
	if err != nil {
		return fmt.Errorf("mtproto: create middle proxy read cipher: %w", err)
	}
	r.encode = cipher.NewCBCEncrypter(writeBlock, writeIV)
	r.decode = cipher.NewCBCDecrypter(readBlock, readIV)

	localPID := processID{port: 0, pid: uint16(os.Getpid()), utime: timestamp}
	localPID.ip = rpcIPv4Bytes(local.IP.To4())
	remotePID := processID{port: int16(remote.Port)}
	remotePID.ip = rpcIPv4Bytes(remote.IP.To4())
	handshake := make([]byte, 32)
	binary.LittleEndian.PutUint32(handshake[0:4], rpcHandshake)
	putProcessID(handshake[8:20], localPID)
	putProcessID(handshake[20:32], remotePID)
	if err := r.writePacketLocked(handshake); err != nil {
		return fmt.Errorf("mtproto: send middle proxy handshake: %w", err)
	}
	_, reply, err := r.readPacket()
	if err != nil {
		return fmt.Errorf("mtproto: read middle proxy handshake: %w", err)
	}
	if err := validateMiddleRPCHandshakeReply(reply, localPID); err != nil {
		return err
	}
	return nil
}

func validateMiddleRPCHandshakeReply(reply []byte, localPID processID) error {
	if len(reply) != 32 || binary.LittleEndian.Uint32(reply[:4]) != rpcHandshake {
		return fmt.Errorf("mtproto: invalid middle proxy handshake response")
	}
	if binary.LittleEndian.Uint32(reply[4:8])&0xff != 0 {
		return fmt.Errorf("mtproto: unsupported middle proxy handshake flags")
	}
	// Telegram's MTProxy sets TCP_RPC_IGNORE_PID on its middle-proxy client. The server may
	// identify the responding worker by an internal address rather than the public socket peer,
	// so authenticating that field against RemoteAddr rejects a valid encrypted handshake. The
	// peer field still has to match our complete process identity below.
	if !processIDMatches(localPID, getProcessID(reply[20:32])) {
		return fmt.Errorf("mtproto: middle proxy handshake peer mismatch")
	}
	return nil
}

func middleRPCKeys(serverNonce, clientNonce [16]byte, timestamp int32, server, client *net.TCPAddr, secret []byte) ([]byte, []byte, []byte, []byte, error) {
	serverIP4, clientIP4 := server.IP.To4(), client.IP.To4()
	serverIP16, clientIP16 := server.IP.To16(), client.IP.To16()
	if serverIP16 == nil || clientIP16 == nil || (serverIP4 == nil) != (clientIP4 == nil) {
		return nil, nil, nil, nil, fmt.Errorf("mtproto: incompatible middle proxy socket address families")
	}
	if clientIP4 != nil && !directPublicIPv4(clientIP4) {
		return nil, nil, nil, nil, fmt.Errorf(
			"mtproto: middle proxy socket source %s is not a direct public IPv4 address",
			client.IP,
		)
	}
	material := make([]byte, 0, 118+len(secret))
	material = append(material, serverNonce[:]...)
	material = append(material, clientNonce[:]...)
	material = binary.LittleEndian.AppendUint32(material, uint32(timestamp))
	if serverIP4 != nil {
		encoded := rpcIPv4Bytes(serverIP4)
		material = append(material, encoded[:]...)
	} else {
		material = append(material, 0, 0, 0, 0)
	}
	material = binary.LittleEndian.AppendUint16(material, uint16(client.Port))
	material = append(material, "CLIENT"...)
	if clientIP4 != nil {
		encoded := rpcIPv4Bytes(clientIP4)
		material = append(material, encoded[:]...)
	} else {
		material = append(material, 0, 0, 0, 0)
	}
	material = binary.LittleEndian.AppendUint16(material, uint16(server.Port))
	material = append(material, secret...)
	material = append(material, serverNonce[:]...)
	if serverIP4 == nil {
		material = append(material, clientIP16...)
		material = append(material, serverIP16...)
	}
	material = append(material, clientNonce[:]...)

	writeKey, writeIV := rpcAESKey(material)
	copy(material[42:48], "SERVER")
	readKey, readIV := rpcAESKey(material)
	for index := range material {
		material[index] = 0
	}
	return writeKey, writeIV, readKey, readIV, nil
}

func directPublicIPv4(ip net.IP) bool {
	ip = ip.To4()
	if ip == nil || !ip.IsGlobalUnicast() || ip.IsPrivate() || ip.IsLoopback() || ip.IsLinkLocalUnicast() {
		return false
	}
	// Go intentionally does not classify RFC 6598 shared address space as private, but it is
	// carrier-grade NAT and therefore cannot supply the socket identity Telegram signs.
	return !(ip[0] == 100 && ip[1]&0xc0 == 64)
}

// The official C implementation stores IPv4 addresses as ntohl(s_addr) and then appends the
// host integer's memory representation. On little-endian hosts this is the reverse of net.IP's
// byte order; keeping the conversion explicit makes the wire compatibility testable.
func rpcIPv4Bytes(ip net.IP) [4]byte {
	var encoded [4]byte
	if ipv4 := ip.To4(); ipv4 != nil {
		encoded[0], encoded[1], encoded[2], encoded[3] = ipv4[3], ipv4[2], ipv4[1], ipv4[0]
	}
	return encoded
}

func rpcAESKey(material []byte) ([]byte, []byte) {
	md5Key := md5.Sum(material[1:])
	sha1Key := sha1.Sum(material)
	key := make([]byte, 32)
	copy(key[:12], md5Key[:12])
	copy(key[12:], sha1Key[:])
	ivHash := md5.Sum(material[2:])
	iv := append([]byte(nil), ivHash[:]...)
	return key, iv
}

func (r *middleRPC) writePacket(payload []byte) error {
	r.writeMu.Lock()
	defer r.writeMu.Unlock()
	return r.writePacketLocked(payload)
}

func (r *middleRPC) writePacketLocked(payload []byte) error {
	frame := rpcFrame(r.outSeq, payload)
	r.outSeq++
	for len(frame)%aes.BlockSize != 0 {
		frame = binary.LittleEndian.AppendUint32(frame, 4)
	}
	r.encode.CryptBlocks(frame, frame)
	return writeAll(r.conn, frame)
}

func (r *middleRPC) readPacket() (int32, []byte, error) {
	for {
		for len(r.plain) < 4 {
			if err := r.readBlock(); err != nil {
				return 0, nil, err
			}
		}
		length := int(binary.LittleEndian.Uint32(r.plain[:4]))
		if length == 4 {
			r.plain = r.plain[4:]
			continue
		}
		if length < 16 || length > maxRPCPayloadSize+rpcFrameOverhead || length%4 != 0 {
			return 0, nil, fmt.Errorf("mtproto: invalid middle proxy frame length %d", length)
		}
		for len(r.plain) < length {
			if err := r.readBlock(); err != nil {
				return 0, nil, err
			}
		}
		frame := append([]byte(nil), r.plain[:length]...)
		r.plain = r.plain[length:]
		sequence, payload, err := parseRPCFrame(frame)
		if err != nil {
			return 0, nil, err
		}
		if sequence != r.inSeq {
			return 0, nil, fmt.Errorf("mtproto: middle proxy sequence %d, expected %d", sequence, r.inSeq)
		}
		r.inSeq++
		return sequence, payload, nil
	}
}

func (r *middleRPC) readBlock() error {
	ciphertext := make([]byte, aes.BlockSize)
	if _, err := io.ReadFull(r.conn, ciphertext); err != nil {
		return err
	}
	r.decode.CryptBlocks(ciphertext, ciphertext)
	r.plain = append(r.plain, ciphertext...)
	return nil
}

func rpcFrame(sequence int32, payload []byte) []byte {
	frame := make([]byte, 8, len(payload)+rpcFrameOverhead)
	binary.LittleEndian.PutUint32(frame[:4], uint32(len(payload)+rpcFrameOverhead))
	binary.LittleEndian.PutUint32(frame[4:8], uint32(sequence))
	frame = append(frame, payload...)
	frame = binary.LittleEndian.AppendUint32(frame, crc32.ChecksumIEEE(frame))
	return frame
}

func readPlainRPCFrame(reader io.Reader) (int32, []byte, error) {
	header := make([]byte, 4)
	if _, err := io.ReadFull(reader, header); err != nil {
		return 0, nil, err
	}
	length := int(binary.LittleEndian.Uint32(header))
	if length < 16 || length > maxRPCPayloadSize+rpcFrameOverhead || length%4 != 0 {
		return 0, nil, fmt.Errorf("invalid frame length %d", length)
	}
	frame := make([]byte, length)
	copy(frame, header)
	if _, err := io.ReadFull(reader, frame[4:]); err != nil {
		return 0, nil, err
	}
	return parseRPCFrame(frame)
}

func parseRPCFrame(frame []byte) (int32, []byte, error) {
	if len(frame) < 16 || int(binary.LittleEndian.Uint32(frame[:4])) != len(frame) {
		return 0, nil, fmt.Errorf("mtproto: malformed middle proxy frame")
	}
	want := binary.LittleEndian.Uint32(frame[len(frame)-4:])
	if got := crc32.ChecksumIEEE(frame[:len(frame)-4]); got != want {
		return 0, nil, fmt.Errorf("mtproto: middle proxy frame checksum mismatch")
	}
	return int32(binary.LittleEndian.Uint32(frame[4:8])), append([]byte(nil), frame[8:len(frame)-4]...), nil
}

func putProcessID(destination []byte, id processID) {
	copy(destination[:4], id.ip[:])
	binary.LittleEndian.PutUint16(destination[4:6], uint16(id.port))
	binary.LittleEndian.PutUint16(destination[6:8], id.pid)
	binary.LittleEndian.PutUint32(destination[8:12], uint32(id.utime))
}

func getProcessID(source []byte) processID {
	var id processID
	copy(id.ip[:], source[:4])
	id.port = int16(binary.LittleEndian.Uint16(source[4:6]))
	id.pid = binary.LittleEndian.Uint16(source[6:8])
	id.utime = int32(binary.LittleEndian.Uint32(source[8:12]))
	return id
}

func processIDMatches(actual, pattern processID) bool {
	return (pattern.ip == [4]byte{} || pattern.ip == actual.ip) &&
		(pattern.port == 0 || pattern.port == actual.port) &&
		(pattern.pid == 0 || pattern.pid == actual.pid) &&
		(pattern.utime == 0 || pattern.utime == actual.utime)
}

func writeAll(writer io.Writer, data []byte) error {
	for len(data) > 0 {
		written, err := writer.Write(data)
		if err != nil {
			return err
		}
		if written == 0 {
			return io.ErrShortWrite
		}
		data = data[written:]
	}
	return nil
}
