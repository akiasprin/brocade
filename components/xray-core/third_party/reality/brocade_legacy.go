package reality

// legacyX25519KeyShare keeps the compatibility path narrower than modern REALITY
// admission: a profile advertising ML-KEM must still supply its complete share.
func legacyX25519KeyShare(hello *clientHelloMsg) []byte {
	classical := false
	for _, group := range hello.supportedCurves {
		if group == X25519MLKEM768 {
			return nil
		}
		classical = classical || group == X25519
	}
	if !classical {
		return nil
	}
	var key []byte
	for _, share := range hello.keyShares {
		if share.group == X25519MLKEM768 {
			return nil
		}
		if share.group == X25519 {
			if key != nil || len(share.data) != 32 {
				return nil
			}
			key = share.data
		}
	}
	return key
}
