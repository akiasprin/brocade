package anytls

import (
	"io"

	"github.com/xtls/xray-core/common/buf"
)

func writeFull(w io.Writer, p []byte) error {
	for len(p) > 0 {
		n, err := w.Write(p)
		if n > 0 {
			p = p[n:]
		}
		if err != nil {
			return err
		}
		if n == 0 {
			return io.ErrShortWrite
		}
	}
	return nil
}

func readMultiBufferExact(br *buf.BufferedReader, length int) (buf.MultiBuffer, error) {
	var mb buf.MultiBuffer
	remaining := length

	for remaining > 0 {
		b := buf.New()

		size := buf.Size
		if remaining < size {
			size = remaining
		}

		p := b.Extend(int32(size))

		if _, err := io.ReadFull(br, p); err != nil {
			b.Release()
			buf.ReleaseMulti(mb)
			return nil, err
		}

		mb = append(mb, b)
		remaining -= size
	}

	return mb, nil
}

func discardBytes(br *buf.BufferedReader, length int) error {
	remaining := length
	b := buf.New()
	defer b.Release()
	for remaining > 0 {
		size := buf.Size
		if remaining < size {
			size = remaining
		}
		b.Clear()
		p := b.Extend(int32(size))
		if _, err := io.ReadFull(br, p); err != nil {
			b.Release()
			return err
		}
		remaining -= size
	}
	return nil
}

func readText(br *buf.BufferedReader, length int) (string, error) {
	if length <= 0 {
		return "", nil
	}
	body := buf.NewWithSize(int32(length))
	bodyBytes := body.Extend(int32(length))
	if _, err := io.ReadFull(br, bodyBytes); err != nil {
		body.Release()
		return "", err
	}
	text := string(bodyBytes)
	body.Release()
	return text, nil
}
