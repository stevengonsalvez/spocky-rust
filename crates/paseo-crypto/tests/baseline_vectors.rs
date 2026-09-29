use paseo_crypto::{
    decrypt, derive_shared_key, encrypt, encrypt_with_nonce, export_public_key, export_secret_key,
    generate_key_pair, import_public_key, import_secret_key, key_pair_from_secret,
};

const ALICE_SECRET: [u8; 32] =
    hex("000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f");
const ALICE_PUBLIC: [u8; 32] =
    hex("8f40c5adb68f25624ae5b214ea767a6ec94d829d3d7b5e1ad1ba6f3e2138285f");
const BOB_SECRET: [u8; 32] =
    hex("202122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f");
const BOB_PUBLIC: [u8; 32] =
    hex("358072d6365880d1aeea329adf9121383851ed21a28e3b75e965d0d2cd166254");
const SHARED: [u8; 32] = hex("429b61f5d96e37268dfc5114849d599c9ceabffdb68c1f52cd0499af30f5b377");
const NONCE: [u8; 24] = hex("000102030405060708090a0b0c0d0e0f1011121314151617");
const PLAINTEXT: [u8; 11] = hex("0001027f80ff506173656f");
const BUNDLE: [u8; 51] = hex(
    "000102030405060708090a0b0c0d0e0f1011121314151617b5b1f6d6a267a09dc29ab234506ba27163d3bd4ffc8718ce3216d9",
);

#[test]
fn deterministic_keypairs_match_tweetnacl_1_0_3() {
    let alice = key_pair_from_secret(ALICE_SECRET);
    let bob = key_pair_from_secret(BOB_SECRET);
    assert_eq!(alice.public_key, ALICE_PUBLIC);
    assert_eq!(bob.public_key, BOB_PUBLIC);
    assert_eq!(
        export_public_key(&alice.public_key).unwrap(),
        "j0DFrbaPJWJK5bIU6nZ6bslNgp09e14a0bpvPiE4KF8="
    );
    assert_eq!(
        export_public_key(&bob.public_key).unwrap(),
        "NYBy1jZYgNGu6jKa35EhODhR7SGijjt16WXQ0s0WYlQ="
    );
    assert_eq!(
        import_public_key("j0DFrbaPJWJK5bIU6nZ6bslNgp09e14a0bpvPiE4KF8=").unwrap(),
        ALICE_PUBLIC
    );
    assert_eq!(
        import_secret_key(&export_secret_key(&ALICE_SECRET).unwrap()).unwrap(),
        ALICE_SECRET
    );
}

#[test]
fn curve25519_before_key_matches_tweetnacl_1_0_3() {
    assert_eq!(
        derive_shared_key(&ALICE_SECRET, &BOB_PUBLIC).unwrap(),
        SHARED
    );
    assert_eq!(
        derive_shared_key(&BOB_SECRET, &ALICE_PUBLIC).unwrap(),
        SHARED
    );
}

#[test]
fn xsalsa20_poly1305_bundle_matches_tweetnacl_1_0_3() {
    let bundle = encrypt_with_nonce(&SHARED, &NONCE, &PLAINTEXT).unwrap();
    assert_eq!(bundle, BUNDLE);
    assert_eq!(decrypt(&SHARED, &bundle).unwrap(), PLAINTEXT);
}

#[test]
fn generated_keys_and_random_nonce_frames_round_trip() {
    let alice = generate_key_pair();
    let bob = generate_key_pair();
    let shared = derive_shared_key(&alice.secret_key, &bob.public_key).unwrap();
    let first = encrypt(&shared, b"Paseo").unwrap();
    let second = encrypt(&shared, b"Paseo").unwrap();
    assert_eq!(first.len(), 5 + 40);
    assert_ne!(first, second);
    assert_eq!(decrypt(&shared, &first).unwrap(), b"Paseo");
    assert_eq!(decrypt(&shared, &second).unwrap(), b"Paseo");
}

#[test]
fn rejects_malformed_keys_and_ciphertext_like_the_baseline() {
    assert_eq!(
        import_public_key(&"!".repeat(43)).unwrap_err().to_string(),
        "Invalid public key encoding"
    );
    assert_eq!(
        import_public_key(&format!("{}B=", "A".repeat(42)))
            .unwrap_err()
            .to_string(),
        "Invalid public key encoding"
    );
    assert_eq!(
        export_public_key(&[0; 31]).unwrap_err().to_string(),
        "Invalid public key length (expected 32)"
    );
    assert_eq!(
        derive_shared_key(&ALICE_SECRET, &[0; 32])
            .unwrap_err()
            .to_string(),
        "Invalid peer public key"
    );
    let unsupported_keys: [[u8; 32]; 6] = [
        hex("0100000000000000000000000000000000000000000000000000000000000000"),
        hex("e0eb7a7c3b41b8ae1656e3faf19fc46ada098deb9c32b1fd866205165f49b800"),
        hex("5f9c95bca3508c24b1d0b1559c83ef5b04445cc4581c8e86d8224eddd09f1157"),
        hex("ecffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f"),
        hex("edffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f"),
        hex("eeffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f"),
    ];
    for unsupported in unsupported_keys {
        assert_eq!(
            derive_shared_key(&ALICE_SECRET, &unsupported)
                .unwrap_err()
                .to_string(),
            "Invalid peer public key"
        );
    }
    assert_eq!(
        derive_shared_key(&ALICE_SECRET, &[0; 31])
            .unwrap_err()
            .to_string(),
        "Invalid peer public key length (expected 32)"
    );
    assert_eq!(
        decrypt(&SHARED, &[0; 23]).unwrap_err().to_string(),
        "Ciphertext bundle too short"
    );
    assert_eq!(
        decrypt(&SHARED, &[0; 24]).unwrap_err().to_string(),
        "Decryption failed"
    );
    let mut tampered = BUNDLE;
    tampered[30] ^= 1;
    assert_eq!(
        decrypt(&SHARED, &tampered).unwrap_err().to_string(),
        "Decryption failed"
    );
}

const fn hex<const N: usize>(input: &str) -> [u8; N] {
    let bytes = input.as_bytes();
    let mut output = [0; N];
    let mut index = 0;
    while index < N {
        output[index] = (nibble(bytes[index * 2]) << 4) | nibble(bytes[index * 2 + 1]);
        index += 1;
    }
    output
}

const fn nibble(byte: u8) -> u8 {
    match byte {
        b'0'..=b'9' => byte - b'0',
        b'a'..=b'f' => byte - b'a' + 10,
        _ => panic!("invalid hex fixture"),
    }
}
