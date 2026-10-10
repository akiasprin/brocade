package mtproto

import (
	"bufio"
	"context"
	"fmt"
	"io"
	"net"
	"net/http"
	"strconv"
	"strings"
	"sync"
	"time"
)

const (
	officialProxySecretURL = "https://core.telegram.org/getProxySecret"
	officialProxyConfigURL = "https://core.telegram.org/getProxyConfig"
	directoryRefreshPeriod = 24 * time.Hour
	maxProxySecretBytes    = 256
	maxProxyConfigBytes    = 1 << 20
)

type proxyTarget struct {
	host string
	port uint16
}

type proxyDirectorySnapshot struct {
	secret    []byte
	defaultDC int16
	targets   map[int16][]proxyTarget
}

type proxyDirectory struct {
	mu sync.Mutex

	client        *http.Client
	secretURL     string
	configURL     string
	refreshPeriod time.Duration
	now           func() time.Time

	snapshot  *proxyDirectorySnapshot
	refreshed time.Time
}

func newOfficialProxyDirectory() *proxyDirectory {
	return &proxyDirectory{
		client:        &http.Client{Timeout: 10 * time.Second},
		secretURL:     officialProxySecretURL,
		configURL:     officialProxyConfigURL,
		refreshPeriod: directoryRefreshPeriod,
		now:           time.Now,
	}
}

func (d *proxyDirectory) target(ctx context.Context, dc int16) (proxyTarget, []byte, error) {
	snapshot, err := d.current(ctx)
	if err != nil {
		return proxyTarget{}, nil, err
	}
	targets := snapshot.targets[dc]
	if len(targets) == 0 {
		targets = snapshot.targets[snapshot.defaultDC]
	}
	if len(targets) == 0 {
		return proxyTarget{}, nil, fmt.Errorf("mtproto: official proxy directory has no target for dc %d", dc)
	}
	// The official implementation randomizes among targets. Rotating by current time keeps the
	// selection cheap while avoiding a permanent preference for the first address.
	index := int(d.now().UnixNano() % int64(len(targets)))
	if index < 0 {
		index = -index
	}
	return targets[index], append([]byte(nil), snapshot.secret...), nil
}

func (d *proxyDirectory) current(ctx context.Context) (*proxyDirectorySnapshot, error) {
	d.mu.Lock()
	defer d.mu.Unlock()

	now := d.now()
	if d.snapshot != nil && now.Sub(d.refreshed) < d.refreshPeriod {
		return d.snapshot, nil
	}
	snapshot, err := d.fetch(ctx)
	if err != nil {
		if d.snapshot != nil {
			return d.snapshot, nil
		}
		return nil, err
	}
	d.snapshot = snapshot
	d.refreshed = now
	return snapshot, nil
}

func (d *proxyDirectory) fetch(ctx context.Context) (*proxyDirectorySnapshot, error) {
	secret, err := d.get(ctx, d.secretURL, maxProxySecretBytes)
	if err != nil {
		return nil, fmt.Errorf("mtproto: fetch official proxy secret: %w", err)
	}
	if len(secret) < 32 || len(secret) > maxProxySecretBytes {
		return nil, fmt.Errorf("mtproto: official proxy secret length %d is outside 32..%d", len(secret), maxProxySecretBytes)
	}
	config, err := d.get(ctx, d.configURL, maxProxyConfigBytes)
	if err != nil {
		return nil, fmt.Errorf("mtproto: fetch official proxy config: %w", err)
	}
	defaultDC, targets, err := parseProxyConfig(string(config))
	if err != nil {
		return nil, err
	}
	return &proxyDirectorySnapshot{secret: secret, defaultDC: defaultDC, targets: targets}, nil
}

func (d *proxyDirectory) get(ctx context.Context, url string, limit int64) ([]byte, error) {
	request, err := http.NewRequestWithContext(ctx, http.MethodGet, url, nil)
	if err != nil {
		return nil, err
	}
	response, err := d.client.Do(request)
	if err != nil {
		return nil, err
	}
	defer response.Body.Close()
	if response.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("unexpected HTTP status %s", response.Status)
	}
	data, err := io.ReadAll(io.LimitReader(response.Body, limit+1))
	if err != nil {
		return nil, err
	}
	if int64(len(data)) > limit {
		return nil, fmt.Errorf("response exceeds %d bytes", limit)
	}
	return data, nil
}

func parseProxyConfig(config string) (int16, map[int16][]proxyTarget, error) {
	var defaultDC int16
	hasDefault := false
	targets := make(map[int16][]proxyTarget)
	scanner := bufio.NewScanner(strings.NewReader(config))
	for scanner.Scan() {
		line := scanner.Text()
		if comment := strings.IndexByte(line, '#'); comment >= 0 {
			line = line[:comment]
		}
		fields := strings.Fields(strings.TrimSpace(line))
		if len(fields) == 0 {
			continue
		}
		switch fields[0] {
		case "default":
			if len(fields) != 2 {
				return 0, nil, fmt.Errorf("mtproto: invalid default directive in official proxy config")
			}
			value, err := parseConfigDC(fields[1])
			if err != nil {
				return 0, nil, err
			}
			defaultDC, hasDefault = value, true
		case "proxy_for":
			if len(fields) != 3 {
				return 0, nil, fmt.Errorf("mtproto: invalid proxy_for directive in official proxy config")
			}
			dc, err := strconv.ParseInt(fields[1], 10, 16)
			if err != nil {
				return 0, nil, fmt.Errorf("mtproto: invalid dc in official proxy config: %w", err)
			}
			address := strings.TrimSuffix(fields[2], ";")
			host, portText, err := net.SplitHostPort(address)
			if err != nil {
				return 0, nil, fmt.Errorf("mtproto: invalid target in official proxy config: %w", err)
			}
			port, err := strconv.ParseUint(portText, 10, 16)
			if err != nil || port == 0 {
				return 0, nil, fmt.Errorf("mtproto: invalid target port in official proxy config")
			}
			targets[int16(dc)] = append(targets[int16(dc)], proxyTarget{host: host, port: uint16(port)})
		}
	}
	if err := scanner.Err(); err != nil {
		return 0, nil, fmt.Errorf("mtproto: read official proxy config: %w", err)
	}
	if !hasDefault {
		return 0, nil, fmt.Errorf("mtproto: official proxy config has no default dc")
	}
	if len(targets) == 0 {
		return 0, nil, fmt.Errorf("mtproto: official proxy config has no targets")
	}
	return defaultDC, targets, nil
}

func parseConfigDC(value string) (int16, error) {
	value = strings.TrimSuffix(value, ";")
	dc, err := strconv.ParseInt(value, 10, 16)
	if err != nil {
		return 0, fmt.Errorf("mtproto: invalid default dc in official proxy config: %w", err)
	}
	return int16(dc), nil
}
