package utils

import "bytes"

func TryTLSBuffers(conn any) (*bytes.Reader, **bytes.Buffer) {
	input, ok := TryAccessField[bytes.Reader](conn, "input")
	if !ok {
		return nil, nil
	}
	if raw, ok := TryAccessField[*bytes.Buffer](conn, "rawInput"); ok {
		return input, raw
	}
	if raw, ok := TryAccessField[bytes.Buffer](conn, "rawInput"); ok {
		return input, &raw
	}
	return nil, nil
}
