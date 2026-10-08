//go:build linux

package tls

import (
	"crypto/tls"
	"encoding/binary"
	"fmt"
	"io"
	"net"
	"runtime"
	"sync"
	"syscall"
	"unsafe"

	"golang.org/x/sys/unix"
)

const (
	kernelTLSVersion13 = 0x0304

	kernelTLSCipherAESGCM128 = 51
	kernelTLSCipherAESGCM256 = 52

	kernelTLSSetTX = 1
	kernelTLSSetRX = 2

	kernelTLSSetRecordType = 1
	kernelTLSGetRecordType = 2

	tlsRecordTypeAlert           = 21
	tlsRecordTypeHandshake       = 22
	tlsRecordTypeApplicationData = 23
	tlsAlertLevelWarning         = 1
	tlsAlertCloseNotify          = 0
)

type kernelTLSConn struct {
	net.Conn
	raw syscall.RawConn
}

func newKernelTLSConn(conn net.Conn) net.Conn {
	syscallConn, ok := conn.(syscall.Conn)
	if !ok {
		return conn
	}
	raw, err := syscallConn.SyscallConn()
	if err != nil {
		return conn
	}
	return &kernelTLSConn{Conn: conn, raw: raw}
}

func (c *kernelTLSConn) Read(p []byte) (int, error) {
	if len(p) == 0 {
		return 0, nil
	}
	var (
		readBytes int
		readErr   error
	)
	rawErr := c.raw.Read(func(fd uintptr) bool {
		var oob [64]byte
		n, oobn, flags, _, err := unix.Recvmsg(int(fd), p, oob[:], 0)
		if err == unix.EAGAIN || err == unix.EWOULDBLOCK {
			return false
		}
		if err != nil {
			readErr = err
			return true
		}
		if flags&unix.MSG_CTRUNC != 0 {
			readErr = fmt.Errorf("kTLS record type control message was truncated")
			return true
		}
		recordType, err := kernelTLSRecordType(oob[:oobn])
		if err != nil {
			readErr = err
			return true
		}
		switch recordType {
		case 0, tlsRecordTypeApplicationData:
			readBytes = n
			if n == 0 {
				readErr = io.EOF
			}
		case tlsRecordTypeAlert:
			if n >= 2 && p[1] == tlsAlertCloseNotify {
				readErr = io.EOF
			} else {
				readErr = fmt.Errorf("kTLS received TLS alert %x", p[:n])
			}
		case tlsRecordTypeHandshake:
			readErr = fmt.Errorf("kTLS received unsupported post-handshake message")
		default:
			readErr = fmt.Errorf("kTLS received unexpected record type %d", recordType)
		}
		return true
	})
	if rawErr != nil {
		return 0, rawErr
	}
	return readBytes, readErr
}

func kernelTLSRecordType(oob []byte) (byte, error) {
	if len(oob) == 0 {
		return 0, nil
	}
	messages, err := unix.ParseSocketControlMessage(oob)
	if err != nil {
		return 0, err
	}
	for _, message := range messages {
		if message.Header.Level == unix.SOL_TLS && message.Header.Type == kernelTLSGetRecordType && len(message.Data) > 0 {
			return message.Data[0], nil
		}
	}
	return 0, nil
}

type kernelTLSCryptoInfoAESGCM128 struct {
	Version    uint16
	CipherType uint16
	IV         [8]byte
	Key        [16]byte
	Salt       [4]byte
	RecordSeq  [8]byte
}

type kernelTLSCryptoInfoAESGCM256 struct {
	Version    uint16
	CipherType uint16
	IV         [8]byte
	Key        [32]byte
	Salt       [4]byte
	RecordSeq  [8]byte
}

type kernelTLSProbe struct {
	once sync.Once
	err  error
}

var kernelTLSProbes = map[uint16]*kernelTLSProbe{
	tls.TLS_AES_128_GCM_SHA256: {},
	tls.TLS_AES_256_GCM_SHA384: {},
}

func preflightKernelTLS() error {
	var failures []error
	for _, cipherSuite := range []uint16{tls.TLS_AES_128_GCM_SHA256, tls.TLS_AES_256_GCM_SHA384} {
		probe := kernelTLSProbes[cipherSuite]
		probe.once.Do(func() {
			probe.err = probeKernelTLSCipher(cipherSuite)
		})
		if probe.err == nil {
			return nil
		}
		failures = append(failures, probe.err)
	}
	return fmt.Errorf("no supported AES-GCM kTLS cipher: %v", failures)
}

func installKernelTLS(conn net.Conn, material *kernelTLSKeyMaterial) (bool, error) {
	probe := kernelTLSProbes[material.cipherSuite]
	if probe == nil {
		return false, fmt.Errorf("unsupported cipher suite 0x%x", material.cipherSuite)
	}
	probe.once.Do(func() {
		probe.err = probeKernelTLSCipher(material.cipherSuite)
	})
	if probe.err != nil {
		return false, probe.err
	}
	return configureKernelTLSSocket(conn, material)
}

func probeKernelTLSCipher(cipherSuite uint16) error {
	listener, err := net.Listen("tcp4", "127.0.0.1:0")
	if err != nil {
		return fmt.Errorf("open kTLS probe listener: %w", err)
	}
	defer listener.Close()

	type acceptResult struct {
		conn net.Conn
		err  error
	}
	accepted := make(chan acceptResult, 1)
	go func() {
		conn, acceptErr := listener.Accept()
		accepted <- acceptResult{conn: conn, err: acceptErr}
	}()
	client, err := net.Dial("tcp4", listener.Addr().String())
	if err != nil {
		return fmt.Errorf("dial kTLS probe socket: %w", err)
	}
	defer client.Close()
	serverResult := <-accepted
	if serverResult.err != nil {
		return fmt.Errorf("accept kTLS probe socket: %w", serverResult.err)
	}
	defer serverResult.conn.Close()

	keyLen := 16
	if cipherSuite == tls.TLS_AES_256_GCM_SHA384 {
		keyLen = 32
	}
	material := &kernelTLSKeyMaterial{
		cipherSuite: cipherSuite,
		txKey:       make([]byte, keyLen),
		txIV:        make([]byte, 12),
		rxKey:       make([]byte, keyLen),
		rxIV:        make([]byte, 12),
	}
	defer material.clear()
	_, err = configureKernelTLSSocket(client, material)
	if err != nil {
		return fmt.Errorf("kernel rejected cipher suite 0x%x: %w", cipherSuite, err)
	}
	return nil
}

func configureKernelTLSSocket(conn net.Conn, material *kernelTLSKeyMaterial) (bool, error) {
	syscallConn, ok := conn.(syscall.Conn)
	if !ok {
		return false, errKernelTLSUnsupportedConn
	}
	rawConn, err := syscallConn.SyscallConn()
	if err != nil {
		return false, err
	}

	modified := false
	var socketErr error
	err = rawConn.Control(func(fd uintptr) {
		if socketErr = unix.SetsockoptString(int(fd), unix.IPPROTO_TCP, unix.TCP_ULP, "tls"); socketErr != nil {
			return
		}
		modified = true
		if socketErr = setKernelTLSCrypto(int(fd), kernelTLSSetTX, material.cipherSuite, material.txKey, material.txIV, material.txRecordSequence); socketErr != nil {
			return
		}
		socketErr = setKernelTLSCrypto(int(fd), kernelTLSSetRX, material.cipherSuite, material.rxKey, material.rxIV, material.rxRecordSequence)
	})
	if err != nil {
		return modified, err
	}
	return modified, socketErr
}

func setKernelTLSCrypto(fd, direction int, cipherSuite uint16, key, iv []byte, recordSequence uint64) error {
	if len(iv) != 12 {
		return fmt.Errorf("invalid TLS 1.3 IV length %d", len(iv))
	}
	var (
		pointer unsafe.Pointer
		size    uintptr
	)
	switch cipherSuite {
	case tls.TLS_AES_128_GCM_SHA256:
		if len(key) != 16 {
			return fmt.Errorf("invalid AES-128-GCM key length %d", len(key))
		}
		info := kernelTLSCryptoInfoAESGCM128{
			Version:    kernelTLSVersion13,
			CipherType: kernelTLSCipherAESGCM128,
		}
		copy(info.Salt[:], iv[:4])
		copy(info.IV[:], iv[4:])
		copy(info.Key[:], key)
		binary.BigEndian.PutUint64(info.RecordSeq[:], recordSequence)
		pointer = unsafe.Pointer(&info)
		size = unsafe.Sizeof(info)
		defer runtime.KeepAlive(info)
	case tls.TLS_AES_256_GCM_SHA384:
		if len(key) != 32 {
			return fmt.Errorf("invalid AES-256-GCM key length %d", len(key))
		}
		info := kernelTLSCryptoInfoAESGCM256{
			Version:    kernelTLSVersion13,
			CipherType: kernelTLSCipherAESGCM256,
		}
		copy(info.Salt[:], iv[:4])
		copy(info.IV[:], iv[4:])
		copy(info.Key[:], key)
		binary.BigEndian.PutUint64(info.RecordSeq[:], recordSequence)
		pointer = unsafe.Pointer(&info)
		size = unsafe.Sizeof(info)
		defer runtime.KeepAlive(info)
	default:
		return fmt.Errorf("unsupported cipher suite 0x%x", cipherSuite)
	}

	_, _, errno := unix.Syscall6(
		unix.SYS_SETSOCKOPT,
		uintptr(fd),
		uintptr(unix.SOL_TLS),
		uintptr(direction),
		uintptr(pointer),
		size,
		0,
	)
	if errno != 0 {
		return errno
	}
	return nil
}

func closeKernelTLS(conn net.Conn) error {
	var alertErr error
	if syscallConn, ok := conn.(syscall.Conn); ok {
		if rawConn, err := syscallConn.SyscallConn(); err == nil {
			oob := make([]byte, unix.CmsgSpace(1))
			header := (*unix.Cmsghdr)(unsafe.Pointer(&oob[0]))
			header.Level = unix.SOL_TLS
			header.Type = kernelTLSSetRecordType
			header.SetLen(unix.CmsgLen(1))
			oob[unix.CmsgLen(0)] = tlsRecordTypeAlert
			payload := []byte{tlsAlertLevelWarning, tlsAlertCloseNotify}
			rawErr := rawConn.Write(func(fd uintptr) bool {
				_, sendErr := unix.SendmsgN(int(fd), payload, oob, nil, 0)
				if sendErr == unix.EAGAIN || sendErr == unix.EWOULDBLOCK {
					return false
				}
				alertErr = sendErr
				return true
			})
			if rawErr != nil {
				alertErr = rawErr
			}
		}
	}
	closeErr := conn.Close()
	if alertErr != nil {
		return alertErr
	}
	return closeErr
}
