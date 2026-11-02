//! DeepSeekHashV1 proof-of-work.
//!
//! The algorithm is SHA3-256 with round 0 of the Keccak-p[1600] permutation
//! skipped: 23 rounds instead of FIPS-202's 24, rate 136 bytes, `0x06` domain
//! suffix, `0x80` final bit. No crate in the tree exposes a configurable round
//! count (`sha3` hardcodes 24), so the permutation is hand-ported from the
//! upstream `noble-hashes` derivative.
//!
//! The search space is `difficulty` (up to 250_000, per upstream's own cap) and
//! each candidate costs one permutation, so the solve is CPU-bound but short —
//! tens of milliseconds at the observed difficulties. It runs inline rather
//! than on a worker because the router has no worker pool and the request is
//! already inside a `spawn_blocking`-free async task; `ponytail:` if the cost
//! ever shows up in a profile, move `find_nonce` to `tokio::task::spawn_blocking`
//! — the function is pure and `Send`.

/// Upstream's own ceiling (`MAX_DEEPSEEK_POW_DIFFICULTY`).
pub const MAX_DIFFICULTY: u32 = 250_000;

/// Keccak-p[1600] round count for this variant. FIPS-202 uses 24.
const ROUNDS: usize = 23;
/// SHA3-256 rate in bytes.
const BLOCK: usize = 136;
/// State size in bytes (1600 bits).
const STATE_BYTES: usize = 200;

/// `SHA3_PI[t]` / `SHA3_ROTL[t]` / the iota constants, built once.
struct RoundConstants {
    pi: [usize; 24],
    rotl: [u32; 24],
    iota_h: [u32; 24],
    iota_l: [u32; 24],
}

impl RoundConstants {
    fn build() -> Self {
        let mut pi = [0usize; 24];
        let mut rotl = [0u32; 24];
        let mut iota_h = [0u32; 24];
        let mut iota_l = [0u32; 24];

        // `R` is an 8-bit LFSR over GF(2^8); bit j of `t` is round-constant bit
        // `2^j - 1`.
        let mut r: u8 = 1;
        let (mut x, mut y) = (1usize, 0usize);
        for round in 0..24usize {
            let nx = y;
            let ny = (2 * x + 3 * y) % 5;
            x = nx;
            y = ny;
            pi[round] = 2 * (5 * y + x);
            rotl[round] = (((round + 1) * (round + 2)) / 2 % 64) as u32;

            let mut t: u64 = 0;
            for j in 0..7u32 {
                r = (r << 1) ^ if r & 0x80 != 0 { 0x71 } else { 0 };
                if r & 2 != 0 {
                    t ^= 1u64 << ((1u64 << j) - 1);
                }
            }
            iota_h[round] = (t & 0xFFFF_FFFF) as u32;
            iota_l[round] = ((t >> 32) & 0xFFFF_FFFF) as u32;
        }

        Self {
            pi,
            rotl,
            iota_h,
            iota_l,
        }
    }
}

fn constants() -> &'static RoundConstants {
    static C: std::sync::LazyLock<RoundConstants> = std::sync::LazyLock::new(RoundConstants::build);
    &C
}

#[inline]
/// The Keccak-p[1600] permutation over a 50-word (200-byte) state, 23 rounds.
///
/// The lane-pair `(h, l)` packing is upstream's: `s[2i]` is the high word and
/// `s[2i + 1]` the low word of lane `i`. Rotations above 32 bits are expressed
/// by swapping which word receives which shift, exactly as the JS does.
fn keccak_p(state: &mut [u32; 50]) {
    let c = constants();
    let mut b = [0u32; 10];

    for round in (24 - ROUNDS)..24 {
        // θ
        for x in 0..10 {
            b[x] = state[x] ^ state[x + 10] ^ state[x + 20] ^ state[x + 30] ^ state[x + 40];
        }
        for x in (0..10).step_by(2) {
            let idx1 = (x + 8) % 10;
            let idx0 = (x + 2) % 10;
            let b0 = b[idx0];
            let b1 = b[idx0 + 1];
            // rotate the 64-bit lane `(b[idx0], b[idx0 + 1])` left by 1, which
            // reaches across both words.
            let (rot_h, rot_l) = rotate_lane(b0, b1, 1);
            let th = rot_h ^ b[idx1];
            let tl = rot_l ^ b[idx1 + 1];
            for y in (0..50).step_by(10) {
                state[x + y] ^= th;
                state[x + y + 1] ^= tl;
            }
        }

        // ρ and π
        let mut cur_h = state[2];
        let mut cur_l = state[3];
        for t in 0..24 {
            let shift = c.rotl[t];
            let (th, tl) = rotate_lane(cur_h, cur_l, shift);
            let pi = c.pi[t];
            cur_h = state[pi];
            cur_l = state[pi + 1];
            state[pi] = th;
            state[pi + 1] = tl;
        }

        // χ
        for y in (0..50).step_by(10) {
            let b0 = state[y];
            let b1 = state[y + 1];
            let b2 = state[y + 2];
            let b3 = state[y + 3];
            state[y] ^= !state[y + 2] & state[y + 4];
            state[y + 1] ^= !state[y + 3] & state[y + 5];
            state[y + 2] ^= !state[y + 4] & state[y + 6];
            state[y + 3] ^= !state[y + 5] & state[y + 7];
            state[y + 4] ^= !state[y + 6] & state[y + 8];
            state[y + 5] ^= !state[y + 7] & state[y + 9];
            state[y + 6] ^= !state[y + 8] & b0;
            state[y + 7] ^= !state[y + 9] & b1;
            state[y + 8] ^= !b0 & b2;
            state[y + 9] ^= !b1 & b3;
        }

        // ι
        state[0] ^= c.iota_h[round];
        state[1] ^= c.iota_l[round];
    }
}

/// Rotate a 64-bit lane held as `(high, low)` left by `shift` (0..64).
#[inline]
fn rotate_lane(h: u32, l: u32, shift: u32) -> (u32, u32) {
    if shift == 0 {
        return (h, l);
    }
    if shift < 32 {
        (
            (h << shift) | (l >> (32 - shift)),
            (l << shift) | (h >> (32 - shift)),
        )
    } else if shift == 32 {
        (l, h)
    } else {
        let s = shift - 32;
        ((l << s) | (h >> (32 - s)), (h << s) | (l >> (32 - s)))
    }
}

/// `keccakP(state)` over a byte view — the same permutation, no packing.
fn permutation_bytes(state: &mut [u8; STATE_BYTES]) {
    // The byte buffer is 200 bytes; copy into the lane array, permute, copy
    // back. `keccak_p` is the only permutation in the file, so the two views
    // cannot drift.
    let mut words = [0u32; 50];
    for (i, word) in words.iter_mut().enumerate() {
        *word = u32::from_le_bytes([
            state[i * 4],
            state[i * 4 + 1],
            state[i * 4 + 2],
            state[i * 4 + 3],
        ]);
    }
    keccak_p(&mut words);
    for (i, word) in words.iter().enumerate() {
        state[i * 4..i * 4 + 4].copy_from_slice(&word.to_le_bytes());
    }
}

/// The DeepSeekHashV1 digest of `text`, as 32 bytes.
///
/// Exposed for the vector test: the solver needs the same function, and a
/// second implementation for the test would prove nothing.
pub fn digest(text: &str) -> [u8; 32] {
    let mut state = [0u8; STATE_BYTES];
    let bytes = text.as_bytes();
    let mut pos = 0usize;
    for byte in bytes {
        state[pos] ^= byte;
        pos += 1;
        if pos == BLOCK {
            permutation_bytes(&mut state);
            pos = 0;
        }
    }
    state[pos] ^= 0x06;
    state[BLOCK - 1] ^= 0x80;
    permutation_bytes(&mut state);

    let mut out = [0u8; 32];
    out.copy_from_slice(&state[..32]);
    out
}

/// Hex digest, lower-case. Used by the tests and by nothing else in the hot
/// path.
pub fn digest_hex(text: &str) -> String {
    digest(text).iter().map(|b| format!("{b:02x}")).collect()
}

/// The smallest nonce whose `prefix + nonce` digest equals `challenge`.
///
/// Returns `None` when the whole search space is exhausted without a hit —
/// upstream returns `-1` there and the caller reports a solver failure.
pub fn find_nonce(prefix: &str, challenge: &[u8; 32], difficulty: u32) -> Option<u32> {
    let prefix_bytes = prefix.as_bytes();
    if prefix_bytes.len() > BLOCK {
        // Upstream absorbs the prefix into the state and only snapshots at a
        // block boundary; a longer prefix would need the same walk. The real
        // salts are far shorter, so this is a guard, not a path.
        return find_nonce_long_prefix(prefix_bytes, challenge, difficulty);
    }

    let mut base = [0u8; STATE_BYTES];
    let mut pos = 0usize;
    for byte in prefix_bytes {
        base[pos] ^= byte;
        pos += 1;
    }

    for nonce in 0..difficulty {
        let mut state = base;
        let mut p = pos;
        for byte in nonce.to_string().bytes() {
            state[p] ^= byte;
            p += 1;
            if p == BLOCK {
                permutation_bytes(&mut state);
                p = 0;
            }
        }
        state[p] ^= 0x06;
        state[BLOCK - 1] ^= 0x80;
        permutation_bytes(&mut state);
        if state[..32] == *challenge {
            return Some(nonce);
        }
    }
    None
}

/// The prefix-major case, where the prefix itself crosses a block boundary.
fn find_nonce_long_prefix(prefix: &[u8], challenge: &[u8; 32], difficulty: u32) -> Option<u32> {
    let mut base = [0u8; STATE_BYTES];
    let mut pos = 0usize;
    for byte in prefix {
        base[pos] ^= byte;
        pos += 1;
        if pos == BLOCK {
            permutation_bytes(&mut base);
            pos = 0;
        }
    }

    for nonce in 0..difficulty {
        let mut state = base;
        let mut p = pos;
        for byte in nonce.to_string().bytes() {
            state[p] ^= byte;
            p += 1;
            if p == BLOCK {
                permutation_bytes(&mut state);
                p = 0;
            }
        }
        state[p] ^= 0x06;
        state[BLOCK - 1] ^= 0x80;
        permutation_bytes(&mut state);
        if state[..32] == *challenge {
            return Some(nonce);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Vectors produced by upstream's own `findDeepSeekPowNonce`
    /// (`open-sse/lib/deepseek-pow-hash.js`) at the ported commit.
    #[test]
    fn digests_match_the_reference_implementation() {
        assert_eq!(
            digest_hex(""),
            "e594808bc5b7151ac160c6d39a02e0a8e261ed588578403099e3561dc40c26b3"
        );
        assert_eq!(
            digest_hex("abc"),
            "f841106c601ce9be9bc38525e90d4178d47f21dd8eb9f238fc55ffaa4ca94506"
        );
        assert_eq!(
            digest_hex("abc_1700000000_"),
            "5c23a968ba5c880aa167cd70c7c172faceebff8fc30b0ea8c5affd0afc7cc1ac"
        );
        assert_eq!(
            digest_hex("abc_1700000000_0"),
            "262bfabaacdd9c115ae3301ab58a714f3bd5e1f72a7eca314ec223baafdba97d"
        );
        assert_eq!(
            digest_hex("salt_1_7"),
            "a792e0e5a144db449c76724b750b1a00c970dc51f586ddd0b200bf69279e2030"
        );
    }

    fn challenge_of(hex: &str) -> [u8; 32] {
        let mut out = [0u8; 32];
        for (i, byte) in out.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).expect("hex vector");
        }
        out
    }

    #[test]
    fn the_solver_finds_the_reference_nonce() {
        // Each challenge is the digest of `salt_1700000000_<nonce>`, so the
        // answer is the nonce it was built from.
        let prefix = "salt_1700000000_";
        for (challenge, nonce) in [
            (
                "eb1554b3f98c63cde3da0be54e8e8d6f0328c6a78ebb096aad42b9d406546c45",
                0u32,
            ),
            (
                "741c5b30bbe8a81ef9acb101308bff64ab34027e2af5b644d93a4a7912a3eed8",
                1,
            ),
            (
                "5013d0ba33b12e18f572058fd0c9bf8d0b8e1d036f9e03c340672f511630b23a",
                7,
            ),
            (
                "48d97ae065f7deeaf76b0a3c5cb8290a5f2d99f26742949c1b543725cb464ba1",
                42,
            ),
            (
                "d823c11fd3c9d83fc1ab9c8a67c9d4fabb770d6f707e434b4dd460d362534459",
                12345,
            ),
        ] {
            assert_eq!(
                find_nonce(prefix, &challenge_of(challenge), 1_000_000),
                Some(nonce),
                "nonce {nonce}"
            );
        }
    }

    #[test]
    fn an_unreachable_challenge_exhausts_the_search_space() {
        let challenge = challenge_of(&"ff".repeat(32));
        assert_eq!(find_nonce("salt_1_", &challenge, 32), None);
    }
}
