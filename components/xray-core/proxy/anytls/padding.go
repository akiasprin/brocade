package anytls

import (
	"crypto/md5"
	"crypto/rand"
	"encoding/binary"
	"fmt"
	"strconv"
	"strings"
)

const CheckMark = -1

const (
	maxPaddingSchemeSize = maxFramePayload
	maxPaddingTargetSize = 4 * 1024 * 1024
)

var defaultPaddingScheme = []byte(`stop=8
0=30-30
1=100-400
2=400-500,c,500-1000,c,500-1000,c,500-1000,c,500-1000
3=9-9,500-1000
4=500-1000
5=500-1000
6=500-1000
7=500-1000`)

type paddingScheme struct {
	rawScheme []byte
	records   map[uint32][]paddingRange
	stop      uint32
	md5       string
}

type paddingRange struct {
	minSize int
	maxSize int
}

func newPaddingScheme(rawScheme []byte) (*paddingScheme, error) {
	if len(rawScheme) == 0 {
		return nil, fmt.Errorf("anytls: empty padding scheme")
	}
	if len(rawScheme) > maxPaddingSchemeSize {
		return nil, fmt.Errorf("anytls: padding scheme too large")
	}
	p := &paddingScheme{
		rawScheme: rawScheme,
		md5:       fmt.Sprintf("%x", md5.Sum(rawScheme)),
		records:   make(map[uint32][]paddingRange),
	}

	seen := make(map[string]struct{})
	stopSeen := false
	for lineNumber, line := range strings.Split(string(rawScheme), "\n") {
		line = strings.TrimSpace(line)
		if line == "" {
			continue
		}
		parts := strings.SplitN(line, "=", 2)
		if len(parts) != 2 {
			return nil, fmt.Errorf("anytls: malformed padding scheme line %d", lineNumber+1)
		}
		key := strings.TrimSpace(parts[0])
		value := strings.TrimSpace(parts[1])
		if key == "" || value == "" {
			return nil, fmt.Errorf("anytls: empty padding scheme token on line %d", lineNumber+1)
		}
		if _, exists := seen[key]; exists {
			return nil, fmt.Errorf("anytls: duplicate padding scheme key %q", key)
		}
		seen[key] = struct{}{}

		if key == "stop" {
			if stopSeen {
				return nil, fmt.Errorf("anytls: duplicate padding scheme stop")
			}
			stopSeen = true
			stop, err := strconv.ParseUint(value, 10, 32)
			if err != nil || strconv.FormatUint(stop, 10) != value {
				return nil, fmt.Errorf("anytls: invalid padding scheme stop")
			}
			p.stop = uint32(stop)
			continue
		}

		packet, err := strconv.ParseUint(key, 10, 32)
		if err != nil || strconv.FormatUint(packet, 10) != key {
			return nil, fmt.Errorf("anytls: invalid padding scheme packet key %q", key)
		}
		maxTargetSize := uint64(maxPaddingTargetSize)
		if packet == 0 {
			maxTargetSize = maxFramePayload
		}
		ranges := make([]paddingRange, 0, strings.Count(value, ",")+1)
		for _, token := range strings.Split(value, ",") {
			token = strings.TrimSpace(token)
			if token == "c" {
				ranges = append(ranges, paddingRange{minSize: CheckMark, maxSize: CheckMark})
				continue
			}
			minText, maxText, found := strings.Cut(token, "-")
			if !found || strings.Contains(maxText, "-") {
				return nil, fmt.Errorf("anytls: invalid padding range %q", token)
			}
			min, minErr := strconv.ParseUint(strings.TrimSpace(minText), 10, 32)
			max, maxErr := strconv.ParseUint(strings.TrimSpace(maxText), 10, 32)
			if minErr != nil || maxErr != nil || min == 0 || max == 0 || min > max || max > maxTargetSize {
				return nil, fmt.Errorf("anytls: invalid padding range %q", token)
			}
			ranges = append(ranges, paddingRange{minSize: int(min), maxSize: int(max)})
		}
		p.records[uint32(packet)] = ranges
	}

	if !stopSeen {
		return nil, fmt.Errorf("anytls: padding scheme stop is missing")
	}

	return p, nil
}

var parsedDefaultPaddingScheme = func() *paddingScheme {
	p, err := newPaddingScheme(defaultPaddingScheme)
	if err != nil {
		panic(err)
	}
	return p
}()

func getDefaultPaddingScheme() *paddingScheme {
	return parsedDefaultPaddingScheme
}

func parsePaddingScheme(schemeStr string) (*paddingScheme, error) {
	if schemeStr == "" {
		return nil, fmt.Errorf("anytls: empty padding scheme")
	}
	return newPaddingScheme([]byte(schemeStr))
}

func (p *paddingScheme) GenerateRecordPayloadSizes(pkt uint32) []int {
	if p == nil {
		return nil
	}
	ranges := p.records[pkt]
	if len(ranges) == 0 {
		return nil
	}
	return p.appendRecordPayloadSizes(make([]int, 0, len(ranges)), pkt)
}

func (p *paddingScheme) appendRecordPayloadSizes(dst []int, pkt uint32) []int {
	ranges := p.records[pkt]
	if len(ranges) == 0 {
		return dst
	}

	for index := range ranges {
		item := &ranges[index]
		dst = append(dst, item.payloadSize())
	}
	return dst
}

func (r *paddingRange) payloadSize() int {
	if r.minSize == CheckMark || r.maxSize <= r.minSize {
		return r.minSize
	}
	limit := uint32(r.maxSize - r.minSize + 1)
	return r.minSize + int(randomPaddingOffset(limit))
}

// randomPaddingOffset uses rejection sampling so every value below limit has
// exactly the same probability. Padding sizes are fingerprinting material, so
// they continue to use the process cryptographic random source.
func randomPaddingOffset(limit uint32) uint32 {
	if limit <= 1 {
		return 0
	}
	threshold := -limit % limit
	for {
		var raw [4]byte
		_, _ = rand.Read(raw[:])
		value := binary.LittleEndian.Uint32(raw[:])
		if value >= threshold {
			return value % limit
		}
	}
}

func getPadding0Size(scheme *paddingScheme) uint16 {
	if scheme == nil {
		return 30
	}

	ranges := scheme.records[0]
	if len(ranges) > 0 {
		size := ranges[0].payloadSize()
		if size > 0 && size <= maxFramePayload {
			return uint16(size)
		}
	}

	return 30
}
