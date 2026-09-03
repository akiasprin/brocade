package anytls

import (
	"bytes"
	"fmt"
	"io"
	"net/http"
	"sort"
	"strings"
)

type masquerade struct {
	statusCode int
	headers    map[string]string
	content    []byte
}

func ValidateMasquerade(config *Masquerade) error {
	_, err := newMasquerade(config)
	return err
}

func newMasquerade(config *Masquerade) (*masquerade, error) {
	if config == nil {
		return defaultMasquerade404(), nil
	}

	typ := strings.ToLower(strings.TrimSpace(config.Type))
	if typ == "" {
		if config.Content != "" || config.StatusCode != 0 || len(config.Headers) > 0 {
			return nil, fmt.Errorf("masquerade type is required")
		}
		return defaultMasquerade404(), nil
	}

	m := &masquerade{
		headers: make(map[string]string, len(config.Headers)),
	}
	for key, value := range config.Headers {
		if !validHeaderName(key) {
			return nil, fmt.Errorf("invalid masquerade header name %q", key)
		}
		if strings.ContainsAny(value, "\r\n") {
			return nil, fmt.Errorf("invalid masquerade header value for %q", key)
		}
		m.headers[key] = value
	}

	switch typ {
	case "404":
		m = defaultMasquerade404()
		for key, value := range config.Headers {
			m.headers[key] = value
		}
	case "string":
		m.statusCode = int(config.StatusCode)
		if m.statusCode == 0 {
			m.statusCode = http.StatusOK
		}
		if m.statusCode < 200 || m.statusCode > 599 {
			return nil, fmt.Errorf("masquerade statusCode must be between 200 and 599")
		}
		m.content = []byte(config.Content)
	default:
		return nil, fmt.Errorf("unknown masquerade type %q", config.Type)
	}

	return m, nil
}

func defaultMasquerade404() *masquerade {
	return &masquerade{
		statusCode: http.StatusNotFound,
		headers: map[string]string{
			"Content-Type":           "text/plain; charset=utf-8",
			"X-Content-Type-Options": "nosniff",
		},
		content: []byte("404 page not found\n"),
	}
}

func validHeaderName(name string) bool {
	if name == "" {
		return false
	}
	for _, r := range name {
		switch {
		case r >= 'a' && r <= 'z', r >= 'A' && r <= 'Z', r >= '0' && r <= '9':
		case strings.ContainsRune("!#$%&'*+-.^_`|~", r):
		default:
			return false
		}
	}
	return true
}

func (m *masquerade) write(w io.Writer) error {
	if m == nil {
		return nil
	}

	statusText := http.StatusText(m.statusCode)
	if statusText == "" {
		statusText = "Status"
	}

	keys := make([]string, 0, len(m.headers))
	contentLength := false
	connection := false
	for key := range m.headers {
		keys = append(keys, key)
		switch strings.ToLower(key) {
		case "content-length":
			contentLength = true
		case "connection":
			connection = true
		}
	}
	sort.Strings(keys)

	var response bytes.Buffer
	fmt.Fprintf(&response, "HTTP/1.1 %d %s\r\n", m.statusCode, statusText)
	for _, key := range keys {
		fmt.Fprintf(&response, "%s: %s\r\n", key, m.headers[key])
	}
	if !contentLength {
		fmt.Fprintf(&response, "Content-Length: %d\r\n", len(m.content))
	}
	if !connection {
		response.WriteString("Connection: close\r\n")
	}
	response.WriteString("\r\n")
	_, _ = response.Write(m.content)
	return writeFull(w, response.Bytes())
}
