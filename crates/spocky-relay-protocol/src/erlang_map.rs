//! The order `Map.keys/1` returns for a map of binary keys on OTP 29.
//!
//! `sync` frames list the keys of the Owner's client map, so their order is part of the
//! raw wire. A map of up to 32 keys is a sorted flat map. A larger map is a hash array
//! mapped trie (`erl_map.c`) whose nodes consume the key hash four bits at a time from
//! the low end and list their children by slot, so keys come out in hash order.
//!
//! Known gap, reachable by an attacker: a v2 `connectionId` is chosen by the client (up to
//! 256 bytes), so a client can pick keys whose 64-bit hashes are equal. The BEAM stores
//! those in a collision node whose internal order this port does not reproduce (it sorts
//! them by key). Finding such a pair costs about 2^32 hash evaluations. Trie depth is
//! verified against the BEAM to level 10 with keys that share their low 40 hash bits.

use std::collections::BTreeSet;

/// Largest map `erts` keeps as a flat map (`MAP_SMALL_MAP_LIMIT`).
const SMALL_MAP_LIMIT: usize = 32;

const C1: u64 = 0x87C3_7B91_1142_53D5;
const C2: u64 = 0x4CF5_AD43_2745_937F;
const TYPE_BINARY: u64 = 12;
const HASH_BITS: u32 = 64;

/// The keys of a map built from `ids`, duplicates collapsed, in `Map.keys/1` order.
#[must_use]
pub fn keys(ids: &[&[u8]]) -> Vec<Vec<u8>> {
    let unique: BTreeSet<&[u8]> = ids.iter().copied().collect();
    if unique.len() <= SMALL_MAP_LIMIT {
        return unique.into_iter().map(<[u8]>::to_vec).collect();
    }
    let hashed = unique.into_iter().map(|key| (hash(key), key)).collect();
    let mut ordered = Vec::new();
    descend(hashed, 0, &mut ordered);
    ordered
}

fn descend(items: Vec<(u64, &[u8])>, level: u32, ordered: &mut Vec<Vec<u8>>) {
    if let [(_, key)] = items.as_slice() {
        ordered.push((*key).to_vec());
        return;
    }
    // ponytail: identical 64-bit hashes form a collision node in the BEAM; they are
    // listed here in key order. Needs a key pair that collides to matter.
    if level * 4 >= HASH_BITS {
        ordered.extend(items.into_iter().map(|(_, key)| key.to_vec()));
        return;
    }
    let mut slots: [Vec<(u64, &[u8])>; 16] = Default::default();
    for item in items {
        let slot = usize::try_from((item.0 >> (4 * level)) & 0xf).unwrap_or(0);
        slots[slot].push(item);
    }
    for slot in slots {
        if !slot.is_empty() {
            descend(slot, level + 1, ordered);
        }
    }
}

/// `erts_internal_hash/1` of a binary: the 128-bit MurmurHash3-style fold of
/// `make_internal_hash` with salt zero. Public so tests can search for keys whose hashes
/// share many low bits and therefore sit deep in the trie.
#[must_use]
pub fn hash(bytes: &[u8]) -> u64 {
    let mut state = State::default();
    state.alpha(TYPE_BINARY);
    state.beta(u64::try_from(bytes.len()).unwrap_or(u64::MAX) * 8);
    let mut blocks = bytes.chunks_exact(16);
    for block in blocks.by_ref() {
        state.alpha(read_u64(&block[..8]));
        state.beta(read_u64(&block[8..]));
    }
    let tail = blocks.remainder();
    if tail.len() > 8 {
        let value = read_u64(&tail[8..])
            .wrapping_mul(C2)
            .rotate_left(33)
            .wrapping_mul(C1);
        state.beta_xor(value);
    }
    if !tail.is_empty() {
        let value = read_u64(&tail[..tail.len().min(8)])
            .wrapping_mul(C1)
            .rotate_left(31)
            .wrapping_mul(C2);
        state.alpha_xor(value);
    }
    state.finish()
}

fn read_u64(bytes: &[u8]) -> u64 {
    bytes.iter().enumerate().fold(0, |value, (index, byte)| {
        value | (u64::from(*byte) << (8 * index))
    })
}

#[derive(Default)]
struct State {
    alpha: u64,
    beta: u64,
    ticks: u64,
}

impl State {
    fn alpha(&mut self, value: u64) {
        self.alpha ^= value.wrapping_mul(C1).rotate_left(31).wrapping_mul(C2);
        self.alpha = self.alpha.rotate_left(27).wrapping_add(self.beta);
        self.alpha = self.alpha.wrapping_mul(5).wrapping_add(0x52DC_E729);
        self.ticks += 1;
    }

    fn beta(&mut self, value: u64) {
        self.beta ^= value.wrapping_mul(C2).rotate_left(33).wrapping_mul(C1);
        self.beta = self.beta.rotate_left(31).wrapping_add(self.alpha);
        self.beta = self.beta.wrapping_mul(5).wrapping_add(0x3849_5AB5);
        self.ticks += 1;
    }

    fn alpha_xor(&mut self, value: u64) {
        self.alpha ^= value;
    }

    fn beta_xor(&mut self, value: u64) {
        self.beta ^= value;
    }

    fn finish(self) -> u64 {
        let mut alpha = self.alpha ^ self.ticks;
        let mut beta = self.beta ^ self.ticks;
        alpha = alpha.wrapping_add(beta);
        beta = beta.wrapping_add(alpha);
        alpha = finalize(alpha);
        beta = finalize(beta);
        alpha = alpha.wrapping_add(beta);
        beta = beta.wrapping_add(alpha);
        alpha ^ beta
    }
}

fn finalize(mut hash: u64) -> u64 {
    hash ^= hash >> 33;
    hash = hash.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
    hash ^= hash >> 33;
    hash = hash.wrapping_mul(0xC4CE_B9FE_1A85_EC53);
    hash ^ (hash >> 33)
}
