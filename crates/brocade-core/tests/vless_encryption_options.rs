use brocade_core::model::{VlessEncryption, VlessEncryptionAppearance, VlessEncryptionOptions};

#[test]
fn complete_settings_are_required_and_keep_wire_encoding() {
    let options = VlessEncryptionOptions::default();
    assert_eq!(options.appearance, VlessEncryptionAppearance::Random);
    let wire: VlessEncryption = serde_json::from_value(serde_json::json!({
        "port": 13800,
        "options": options,
        "private_key": "private",
        "public_key": "public"
    }))
    .unwrap();
    assert_eq!(wire.options, options);
    assert_eq!(
        wire.options.decryption("private"),
        "mlkem768x25519plus.random.600s.private"
    );
    assert_eq!(
        wire.options.encryption("public"),
        "mlkem768x25519plus.random.0rtt.public"
    );
    assert!(
        serde_json::from_value::<VlessEncryption>(serde_json::json!({
            "port": 13800,
            "private_key": "private",
            "public_key": "public"
        }))
        .is_err()
    );
}

#[test]
fn validates_ticket_and_padding_boundaries_before_core_start() {
    for ticket in ["0s", "600s", "1-65535s", "65535s"] {
        let options = VlessEncryptionOptions {
            ticket_lifetime: ticket.to_owned(),
            ..Default::default()
        };
        assert!(options.validate().is_ok(), "{ticket}");
    }
    for ticket in [
        "", "600", "-1s", "0-50s", "600-100s", "65536s", "1-2-3s", "NaNs",
    ] {
        let options = VlessEncryptionOptions {
            ticket_lifetime: ticket.to_owned(),
            ..Default::default()
        };
        assert!(options.validate().is_err(), "{ticket}");
    }
    for padding in [
        "",
        "100-35-35",
        "100-111-1111.75-0-111.50-0-3333",
        "100-35-65553",
    ] {
        assert!(
            VlessEncryptionOptions::validate_padding(padding).is_ok(),
            "{padding}"
        );
    }
    for padding in [
        "99-35-40",
        "100-0-100",
        "100-50-35",
        "100-35-65554",
        "100-35-40.50-0-1",
        "100-35-40.101-0-1.50-0-50",
        "100-35-40.-1-0-1.50-0-50",
        "100-35-40.publickey",
    ] {
        assert!(
            VlessEncryptionOptions::validate_padding(padding).is_err(),
            "{padding}"
        );
    }
}
