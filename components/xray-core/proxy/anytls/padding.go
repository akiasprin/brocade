package anytls

import (
	"crypto/md5"
	"crypto/rand"
	"fmt"
	"math/big"
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
	scheme    map[string]string
	stop      uint32
	md5       string
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
	}

	scheme := make(map[string]string)
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
		if _, exists := scheme[key]; exists {
			return nil, fmt.Errorf("anytls: duplicate padding scheme key %q", key)
		}

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
			scheme[key] = value
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
		for _, token := range strings.Split(value, ",") {
			token = strings.TrimSpace(token)
			if token == "c" {
				continue
			}
			rangeParts := strings.Split(token, "-")
			if len(rangeParts) != 2 {
				return nil, fmt.Errorf("anytls: invalid padding range %q", token)
			}
			min, minErr := strconv.ParseUint(strings.TrimSpace(rangeParts[0]), 10, 32)
			max, maxErr := strconv.ParseUint(strings.TrimSpace(rangeParts[1]), 10, 32)
			if minErr != nil || maxErr != nil || min == 0 || max == 0 || min > max || max > maxTargetSize {
				return nil, fmt.Errorf("anytls: invalid padding range %q", token)
			}
		}
		scheme[key] = value
	}

	if !stopSeen {
		return nil, fmt.Errorf("anytls: padding scheme stop is missing")
	}

	p.scheme = scheme
	return p, nil
}

func getDefaultPaddingScheme() *paddingScheme {
	p, err := newPaddingScheme(defaultPaddingScheme)
	if err != nil {
		panic(err)
	}
	return p
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

	pktSizes := []int{}
	key := strconv.Itoa(int(pkt))
	s, ok := p.scheme[key]
	if !ok {
		return pktSizes
	}

	sRanges := strings.Split(s, ",")
	for _, sRange := range sRanges {
		sRange = strings.TrimSpace(sRange)

		if sRange == "c" {
			pktSizes = append(pktSizes, CheckMark)
			continue
		}

		sRangeMinMax := strings.Split(sRange, "-")
		if len(sRangeMinMax) != 2 {
			continue
		}

		_min, err := strconv.ParseInt(sRangeMinMax[0], 10, 64)
		if err != nil {
			continue
		}
		_max, err := strconv.ParseInt(sRangeMinMax[1], 10, 64)
		if err != nil {
			continue
		}

		if _min > _max {
			_min, _max = _max, _min
		}

		if _min <= 0 || _max <= 0 {
			continue
		}

		if _min == _max {
			pktSizes = append(pktSizes, int(_min))
		} else {
			i, _ := rand.Int(rand.Reader, big.NewInt(_max-_min+1))
			pktSizes = append(pktSizes, int(i.Int64()+_min))
		}
	}

	return pktSizes
}

func getPadding0Size(scheme *paddingScheme) uint16 {
	if scheme == nil {
		return 30
	}

	sizes := scheme.GenerateRecordPayloadSizes(0)
	if len(sizes) > 0 && sizes[0] > 0 && sizes[0] <= maxFramePayload {
		return uint16(sizes[0])
	}

	return 30
}
