//! Minimal SHA-1 (FIPS 180-4) for Git object IDs only.
//!
//! SHA-1 repositories name a blob by the SHA-1 of its header and bytes. The
//! observer computes that ID for a working file to tell whether it equals the
//! indexed blob, without starting a Git process per file. It is not used for
//! authentication, secrecy or integrity against an adversary. Written here
//! for the same reason as `sha256.rs`; see docs/dependencies.md.

pub fn digest(parts: &[&[u8]]) -> [u8; 20] {
    let mut state: [u32; 5] = [0x67452301, 0xefcdab89, 0x98badcfe, 0x10325476, 0xc3d2e1f0];
    let length: usize = parts.iter().map(|part| part.len()).sum();
    let mut block = [0_u8; 64];
    let mut filled = 0;
    let mut compress = |block: &[u8; 64]| {
        let mut w = [0u32; 80];
        for (i, word) in block.as_chunks::<4>().0.iter().enumerate() {
            w[i] = u32::from_be_bytes(*word);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = state;
        for (i, word) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5a827999),
                20..=39 => (b ^ c ^ d, 0x6ed9eba1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8f1bbcdc),
                _ => (b ^ c ^ d, 0xca62c1d6),
            };
            let t = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(*word);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = t;
        }
        for (slot, value) in state.iter_mut().zip([a, b, c, d, e]) {
            *slot = slot.wrapping_add(value);
        }
    };
    let mut feed = |bytes: &[u8], filled: &mut usize, block: &mut [u8; 64]| {
        for byte in bytes {
            block[*filled] = *byte;
            *filled += 1;
            if *filled == 64 {
                compress(block);
                *filled = 0;
            }
        }
    };
    for part in parts {
        feed(part, &mut filled, &mut block);
    }
    let mut tail = vec![0x80_u8];
    tail.resize((119 - filled) % 64 + 1, 0);
    tail.extend_from_slice(&((length as u64).wrapping_mul(8)).to_be_bytes());
    feed(&tail, &mut filled, &mut block);
    debug_assert_eq!(filled, 0);
    let mut out = [0u8; 20];
    for (chunk, word) in out.as_chunks_mut::<4>().0.iter_mut().zip(state) {
        chunk.copy_from_slice(&word.to_be_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sha256::hex;

    // FIPS 180-4 / NIST CAVP example vectors, including the two-block
    // boundary, and a Git blob ID checked against `git hash-object`.
    #[test]
    fn matches_published_vectors_and_git() {
        let cases: [(&[&[u8]], &str); 5] = [
            (&[b""], "da39a3ee5e6b4b0d3255bfef95601890afd80709"),
            (&[b"abc"], "a9993e364706816aba3e25717850c26c9cd0d89d"),
            (
                &[b"abcdbcdecdefdefgefghfghighijhijkijkljklm", b"klmnlmnomnopnopq"],
                "84983e441c3bd26ebaae4aa1f95129e5e54670f1",
            ),
            (
                &[b"abcdefghbcdefghicdefghijdefghijkefghijklfghijklmghijklmnhijklmnoijklmnopjklmnopqklmnopqrlmnopqrsmnopqrstnopqrstu"],
                "a49b2446a02c645bf419f995b67091253a04a259",
            ),
            (
                &[b"blob 6\0", b"hello\n"],
                "ce013625030ba8dba906f756967f9e9ca394464a",
            ),
        ];
        for (input, expected) in cases {
            assert_eq!(hex(&digest(input)), expected);
        }
        let million = vec![b'a'; 1_000_000];
        assert_eq!(
            hex(&digest(&[&million])),
            "34aa973cd4c4daa4f61eeb2bdbad27316534016f"
        );
    }
}
