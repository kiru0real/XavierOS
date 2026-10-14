//! Small, allocation-free SHA-256 and PBKDF2-HMAC-SHA256 implementation for account hashes.
//! Password hashes are salted and deliberately slow; this is still hobby-OS crypto, not audited.

const K: [u32; 64] = [
    0x428a2f98,0x71374491,0xb5c0fbcf,0xe9b5dba5,0x3956c25b,0x59f111f1,0x923f82a4,0xab1c5ed5,
    0xd807aa98,0x12835b01,0x243185be,0x550c7dc3,0x72be5d74,0x80deb1fe,0x9bdc06a7,0xc19bf174,
    0xe49b69c1,0xefbe4786,0x0fc19dc6,0x240ca1cc,0x2de92c6f,0x4a7484aa,0x5cb0a9dc,0x76f988da,
    0x983e5152,0xa831c66d,0xb00327c8,0xbf597fc7,0xc6e00bf3,0xd5a79147,0x06ca6351,0x14292967,
    0x27b70a85,0x2e1b2138,0x4d2c6dfc,0x53380d13,0x650a7354,0x766a0abb,0x81c2c92e,0x92722c85,
    0xa2bfe8a1,0xa81a664b,0xc24b8b70,0xc76c51a3,0xd192e819,0xd6990624,0xf40e3585,0x106aa070,
    0x19a4c116,0x1e376c08,0x2748774c,0x34b0bcb5,0x391c0cb3,0x4ed8aa4a,0x5b9cca4f,0x682e6ff3,
    0x748f82ee,0x78a5636f,0x84c87814,0x8cc70208,0x90befffa,0xa4506ceb,0xbef9a3f7,0xc67178f2,
];

pub const PASSWORD_ITERATIONS: u32 = 50_000;

pub fn sha256(input: &[u8]) -> [u8; 32] {
    // All callers feed short, bounded messages (at most 96 bytes for HMAC). Keep this
    // stack-only to avoid requiring the kernel heap during authentication.
    let mut padded = [0u8; 128];
    let input_len = input.len().min(119);
    padded[..input_len].copy_from_slice(&input[..input_len]);
    padded[input_len] = 0x80;
    let blocks = if input_len <= 55 { 1 } else { 2 };
    let bits = (input_len as u64).wrapping_mul(8).to_be_bytes();
    let length_offset = blocks * 64 - 8;
    padded[length_offset..length_offset + 8].copy_from_slice(&bits);

    let mut h = [
        0x6a09e667u32, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a,
        0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
    ];
    for block_index in 0..blocks {
        let block = &padded[block_index * 64..(block_index + 1) * 64];
        let mut w = [0u32; 64];
        for i in 0..16 {
            let p = i * 4;
            w[i] = u32::from_be_bytes([block[p], block[p + 1], block[p + 2], block[p + 3]]);
        }
        for i in 16..64 {
            let x = w[i - 15];
            let y = w[i - 2];
            let s0 = x.rotate_right(7) ^ x.rotate_right(18) ^ (x >> 3);
            let s1 = y.rotate_right(17) ^ y.rotate_right(19) ^ (y >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let mut a = h[0]; let mut b = h[1]; let mut c = h[2]; let mut d = h[3];
        let mut e = h[4]; let mut f = h[5]; let mut g = h[6]; let mut hh = h[7];
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let choose = (e & f) ^ ((!e) & g);
            let t1 = hh.wrapping_add(s1).wrapping_add(choose).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(majority);
            hh = g; g = f; f = e; e = d.wrapping_add(t1);
            d = c; c = b; b = a; a = t1.wrapping_add(t2);
        }
        h[0] = h[0].wrapping_add(a); h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c); h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e); h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g); h[7] = h[7].wrapping_add(hh);
    }
    let mut out = [0u8; 32];
    for (i, word) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    let mut key_block = [0u8; 64];
    if key.len() > 64 {
        key_block[..32].copy_from_slice(&sha256(key));
    } else {
        key_block[..key.len()].copy_from_slice(key);
    }
    let mut inner = [0u8; 128];
    for i in 0..64 { inner[i] = key_block[i] ^ 0x36; }
    let message_len = message.len().min(64);
    inner[64..64 + message_len].copy_from_slice(&message[..message_len]);
    let inner_hash = sha256(&inner[..64 + message_len]);

    let mut outer = [0u8; 96];
    for i in 0..64 { outer[i] = key_block[i] ^ 0x5c; }
    outer[64..96].copy_from_slice(&inner_hash);
    sha256(&outer)
}

pub fn pbkdf2_sha256(password: &[u8], salt: &[u8; 16], iterations: u32) -> [u8; 32] {
    pbkdf2_with_salt(password, salt, iterations)
}

fn pbkdf2_with_salt(password: &[u8], salt: &[u8], iterations: u32) -> [u8; 32] {
    if iterations == 0 || salt.len() > 64 { return [0u8; 32]; }
    let mut first = [0u8; 68];
    first[..salt.len()].copy_from_slice(salt);
    first[salt.len()..salt.len() + 4].copy_from_slice(&1u32.to_be_bytes());
    let mut u = hmac_sha256(password, &first[..salt.len() + 4]);
    let mut result = u;
    for _ in 1..iterations {
        u = hmac_sha256(password, &u);
        for i in 0..32 { result[i] ^= u[i]; }
    }
    result
}

pub fn constant_time_eq(left: &[u8; 32], right: &[u8; 32]) -> bool {
    let mut difference = 0u8;
    for i in 0..32 { difference |= left[i] ^ right[i]; }
    difference == 0
}

pub fn self_test() -> Result<(), &'static str> {
    const ABC: [u8; 32] = [
        0xba,0x78,0x16,0xbf,0x8f,0x01,0xcf,0xea,0x41,0x41,0x40,0xde,0x5d,0xae,0x22,0x23,
        0xb0,0x03,0x61,0xa3,0x96,0x17,0x7a,0x9c,0xb4,0x10,0xff,0x61,0xf2,0x00,0x15,0xad,
    ];
    if sha256(b"abc") != ABC { return Err("SHA-256 SELF-TEST FAILED"); }
    const PBKDF2_ONE: [u8; 32] = [
        0x12,0x0f,0xb6,0xcf,0xfc,0xf8,0xb3,0x2c,0x43,0xe7,0x22,0x52,0x56,0xc4,0xf8,0x37,
        0xa8,0x65,0x48,0xc9,0x2c,0xcc,0x35,0x48,0x08,0x05,0x98,0x7c,0xb7,0x0b,0xe1,0x7b,
    ];
    if pbkdf2_with_salt(b"password", b"salt", 1) != PBKDF2_ONE {
        return Err("PBKDF2-HMAC-SHA256 SELF-TEST FAILED");
    }
    const PBKDF2_TWO: [u8; 32] = [
        0xae,0x4d,0x0c,0x95,0xaf,0x6b,0x46,0xd3,0x2d,0x0a,0xdf,0xf9,0x28,0xf0,0x6d,0xd0,
        0x2a,0x30,0x3f,0x8e,0xf3,0xc2,0x51,0xdf,0xd6,0xe2,0xd8,0x5a,0x95,0x47,0x4c,0x43,
    ];
    if pbkdf2_with_salt(b"password", b"salt", 2) != PBKDF2_TWO {
        return Err("PBKDF2 ITERATION SELF-TEST FAILED");
    }
    Ok(())
}
