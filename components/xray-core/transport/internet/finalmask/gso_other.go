//go:build !linux

package finalmask

func udpSegmentSize([]byte) (int, error) {
	return 0, nil
}

func adjustUDPSegmentSize(oob []byte, _ int) ([]byte, error) {
	return oob, nil
}
