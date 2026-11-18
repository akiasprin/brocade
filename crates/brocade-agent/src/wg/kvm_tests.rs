//! Exercise production apply/recovery only inside an explicitly isolated KVM netns.
use std::{fs, path::PathBuf};

use crate::{command::run_command_with_input, run_command};

#[test]
#[ignore = "requires Ubuntu 26.04 KVM, enforced wg profiles and a fresh private network namespace"]
fn ordinary_apply_and_recovery_with_enforced_profiles() {
    assert_eq!(
        std::env::var("BROCADE_RUN_WG_KVM_TESTS").as_deref(),
        Ok("1")
    );
    assert!(fs::read_to_string("/etc/os-release")
        .unwrap()
        .contains("26.04"));
    assert!(fs::read_to_string("/sys/kernel/security/apparmor/profiles")
        .unwrap()
        .contains("wg (enforce)"));
    assert_eq!(
        run_command("ip", &["-o", "link", "show"])
            .unwrap()
            .lines()
            .count(),
        1,
        "use unshare -n: the namespace must initially contain only lo"
    );
    let root = PathBuf::from(
        std::env::var("BROCADE_WG_KVM_STATE").expect("explicit disposable state directory"),
    );
    assert!(root.starts_with("/root/") && !root.exists());
    fs::create_dir_all(&root).unwrap();
    let path = root.join("wireguard.conf");
    let private = run_command("wg", &["genkey"]).unwrap();
    let public = run_command_with_input("wg", &["pubkey"], private.as_bytes()).unwrap();
    let peer_private = run_command("wg", &["genkey"]).unwrap();
    let peer = run_command_with_input("wg", &["pubkey"], peer_private.as_bytes()).unwrap();
    let psk = run_command("wg", &["genpsk"]).unwrap();
    let config = |mtu: u16, endpoint: bool| {
        format!("[Interface]\nPrivateKey = {}\nListenPort = 51820\nAddress = 10.223.0.1/24\nMTU = {mtu}\nDNS = 1.1.1.1\nTable = off\nPostUp = false\nSaveConfig = false\n\n[Peer]\n# peer-fixture\nPublicKey = {}\nPresharedKey = {}\nAllowedIPs = 10.223.0.2/32, 10.223.0.3/32\n{}", private.trim(), peer.trim(), psk.trim(), if endpoint { "Endpoint = 198.51.100.2:51820\nPersistentKeepalive = 25\n" } else { "" })
    };
    crate::write_private(&path, &config(1420, true)).unwrap();
    crate::apply_wireguard(&path).unwrap();
    assert_eq!(
        run_command("wg", &["show", "wg0", "public-key"])
            .unwrap()
            .trim(),
        public.trim()
    );
    assert_eq!(
        run_command("wg", &["show", "wg0", "peers"]).unwrap().trim(),
        peer.trim()
    );
    assert!(run_command("ip", &["-o", "addr", "show", "dev", "wg0"])
        .unwrap()
        .contains("10.223.0.1/24"));
    assert!(run_command("ip", &["route", "get", "10.223.0.2"])
        .unwrap()
        .contains("dev wg0"));
    // sysfs may belong to the parent netns; query this namespace through netlink.
    let link = || {
        serde_json::from_str::<serde_json::Value>(
            &run_command("ip", &["-j", "link", "show", "dev", "wg0"]).unwrap(),
        )
        .unwrap()[0]
            .clone()
    };
    let index = link()["ifindex"].clone();
    let hot = format!(
        "{}{}",
        "# large fleet padding\n".repeat(4000),
        config(1380, true)
    );
    assert!(hot.len() > 64 * 1024);
    crate::write_private(&path, &hot).unwrap();
    crate::apply_wireguard(&path).unwrap();
    assert_eq!(
        link()["ifindex"],
        index,
        "hot updates must keep the interface"
    );
    assert_eq!(link()["mtu"].as_u64(), Some(1380));

    // Preserve normal UDP roaming; reset a passive Phantun peer's former public
    // Endpoint, then replay its complete configuration including preshared key.
    crate::write_private(&path, &config(1380, false)).unwrap();
    crate::apply_wireguard(&path).unwrap();
    assert!(run_command("wg", &["show", "wg0", "endpoints"])
        .unwrap()
        .contains("198.51.100.2:51820"));
    fs::write(
        root.join("phantun.json"),
        r#"{"servers":[{"tcp_port":39743,"peers":["peer-fixture"]}]}"#,
    )
    .unwrap();
    crate::apply_wireguard(&path).unwrap();
    assert!(run_command("wg", &["show", "wg0", "endpoints"])
        .unwrap()
        .contains("(none)"));
    assert_eq!(
        run_command("wg", &["show", "wg0", "peers"]).unwrap().trim(),
        peer.trim()
    );
    assert!(run_command("wg", &["show", "wg0", "preshared-keys"])
        .unwrap()
        .contains(psk.trim()));
    assert!(run_command("wg", &["show", "wg0", "persistent-keepalive"])
        .unwrap()
        .contains("off"));
    assert!(run_command("wg", &["show", "wg0", "allowed-ips"])
        .unwrap()
        .contains("10.223.0.3/32"));

    // A hot UDP -> local Phantun transition renews the active session too.
    crate::write_private(&path, &config(1380, true)).unwrap();
    crate::apply_wireguard(&path).unwrap();
    let local = config(1380, true).replace("198.51.100.2:51820", "127.0.0.1:29000");
    assert_eq!(
        crate::wg::new_phantun_clients(&local).unwrap(),
        vec![peer.trim().to_owned()]
    );
    crate::write_private(&path, &local).unwrap();
    crate::apply_wireguard(&path).unwrap();
    assert!(crate::wg::new_phantun_clients(&local).unwrap().is_empty());
    assert!(run_command("wg", &["show", "wg0", "endpoints"])
        .unwrap()
        .contains("127.0.0.1:29000"));
    assert!(run_command("wg", &["show", "wg0", "preshared-keys"])
        .unwrap()
        .contains(psk.trim()));

    crate::write_private(
        &path,
        "[Interface]\nPrivateKey = INVALID_PRIVATE_KEY_SENTINEL\n",
    )
    .unwrap();
    let error = crate::apply_wireguard(&path).unwrap_err();
    assert!(!error.contains("INVALID_PRIVATE_KEY_SENTINEL"));
    assert!(error.contains("configuration rejected"));
    crate::write_private(&path, &config(1420, false)).unwrap();
    crate::apply_wireguard(&path).unwrap();
    run_command("ip", &["link", "delete", "wg0"]).unwrap();
    crate::apply_wireguard(&path).unwrap();
    assert_eq!(
        run_command("wg", &["show", "wg0", "peers"]).unwrap().trim(),
        peer.trim()
    );
    run_command("ip", &["link", "delete", "wg0"]).unwrap();
    fs::remove_dir_all(root).unwrap();
    println!("validated: cold apply; large hot sync; routes/addresses/MTU; UDP roaming; stale Phantun reset and full replay; private error redaction; recovery and interface rebuild");
}
