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
	if length <= 0 {
		return nil, nil
	}
	mb := make(buf.MultiBuffer, 0, (length+buf.Size-1)/buf.Size)
	remaining := int32(length)
	readAny := false

	for remaining > 0 {
		part, err := br.ReadAtMost(remaining)
		partLen := part.Len()
		if partLen > 0 {
			mb, _ = buf.MergeMulti(mb, part)
			remaining -= partLen
			readAny = true
		}
		if remaining == 0 {
			return mb, nil
		}
		if err != nil {
			buf.ReleaseMulti(part)
			buf.ReleaseMulti(mb)
			if err == io.EOF && readAny {
				return nil, io.ErrUnexpectedEOF
			}
			return nil, err
		}
		if partLen == 0 {
			buf.ReleaseMulti(mb)
			return nil, io.ErrNoProgress
		}
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
