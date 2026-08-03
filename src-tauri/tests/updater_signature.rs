use base64::{engine::general_purpose::STANDARD, Engine as _};
use minisign_verify::{PublicKey, Signature};

const PAYLOAD: &[u8] = include_bytes!("fixtures/updater-signature-payload.txt");
const SIGNATURE_B64: &str = include_str!("fixtures/updater-signature-payload.txt.sig");

fn configured_public_key() -> PublicKey {
    let config: serde_json::Value =
        serde_json::from_str(include_str!("../tauri.conf.json")).expect("valid Tauri config");
    let encoded = config["plugins"]["updater"]["pubkey"]
        .as_str()
        .expect("configured updater public key");
    let decoded = STANDARD.decode(encoded).expect("base64 updater public key");
    let text = String::from_utf8(decoded).expect("UTF-8 updater public key");
    PublicKey::decode(&text).expect("minisign updater public key")
}

fn fixture_signature() -> Signature {
    decode_signature(SIGNATURE_B64)
}

fn decode_signature(encoded: &str) -> Signature {
    let decoded = STANDARD
        .decode(encoded.trim())
        .expect("base64 updater signature");
    let text = String::from_utf8(decoded).expect("UTF-8 updater signature");
    Signature::decode(&text).expect("minisign updater signature")
}

#[test]
fn configured_updater_key_accepts_only_the_signed_fixture_bytes() {
    let public_key = configured_public_key();
    let signature = fixture_signature();
    public_key
        .verify(PAYLOAD, &signature, true)
        .expect("configured key must verify the release signing fixture");

    let mut tampered = PAYLOAD.to_vec();
    tampered[0] ^= 1;
    assert!(public_key.verify(&tampered, &signature, true).is_err());
}

#[test]
#[ignore = "requires JUICE_UPDATER_ARTIFACT and JUICE_UPDATER_SIGNATURE"]
fn locally_built_updater_artifact_matches_the_configured_public_key() {
    let artifact = std::env::var("JUICE_UPDATER_ARTIFACT").expect("updater artifact path");
    let signature = std::env::var("JUICE_UPDATER_SIGNATURE").expect("updater signature path");
    let bytes = std::fs::read(artifact).expect("updater artifact bytes");
    let signature =
        decode_signature(&std::fs::read_to_string(signature).expect("updater signature text"));
    configured_public_key()
        .verify(&bytes, &signature, true)
        .expect("configured key must verify the signed updater artifact");

    #[cfg(windows)]
    {
        let config: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).expect("valid Tauri config");
        let version = config["version"].as_str().expect("configured app version");
        let prepared = agent_juice::update::prepare_verified_installer(&bytes, version)
            .expect("signed updater ProductVersion must match the configured version");
        std::fs::remove_file(prepared).expect("remove prepared updater fixture");
    }
}
