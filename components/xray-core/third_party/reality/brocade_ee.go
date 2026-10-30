package reality

// A cover with empty EncryptedExtensions has no room for the optional SNI ack.
// Omit only that extension when needed; retain the record-length check and all
// authenticated handshake contents in the ordinary record writer.
func fitServerNameAckToRecordedRecord(extensions *encryptedExtensionsMsg, recordedLength, overhead int) error {
	if !extensions.serverNameAck || recordedLength == 0 {
		return nil
	}
	encoded, err := extensions.marshal()
	if err != nil {
		return err
	}
	if recordHeaderLen+len(encoded)+1+overhead > recordedLength {
		extensions.serverNameAck = false
	}
	return nil
}
