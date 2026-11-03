//! Differential tests that execute the simd128 kernels.
//!
//! Each test asserts kernel output against an independent scalar oracle.
//! Natively the kernels run their scalar backends, so these tests pass there
//! too — but they only cover the vector path when this target is built for
//! wasm32 with `target-feature=+simd128` and executed in a wasm runtime (the
//! `rust-wasm-simd` CI lane). The `compile_error` below fails the build
//! otherwise, so a scalar-only run can never pretend to be vector coverage.
//!
//! Case selection mirrors the native differential tests: the yEnc rounds have
//! the same shape as `simd_path_agrees_with_scalar_oracle` and the gf16 rounds
//! the same shape as `kernel_matches_independent_russian_peasant_model`. The
//! gf16 sizes pin the vector boundaries explicitly (whole 8-word chunks and
//! non-multiple-of-8 tails) on top of the mirrored random rounds.

#[cfg(all(target_arch = "wasm32", not(target_feature = "simd128")))]
compile_error!(
    "nntune-test-simd must be built with RUSTFLAGS=\"-C target-feature=+simd128\" \
     so the wasm32 vector dispatch under test is live; a scalar build would pass \
     without executing the simd128 kernels"
);

use nntune_core::par2::gf16;
use nntune_core::yenc::{decode_to, encode_segment, SegmentParams};
#[cfg(target_arch = "wasm32")]
use wasm_bindgen_test::wasm_bindgen_test;

/// Deterministic xorshift64* PRNG (same shape as the native kernel tests) so
/// cases are stable across runs and runtimes.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
}

// ---------------------------------------------------------------------------
// yEnc
// ---------------------------------------------------------------------------

/// Fully-scalar article decoder: the differential oracle for the decoder fast
/// path (structure mirrors `decode_to`, minus the CRC check).
fn scalar_decode_article(article: &[u8], dst: &mut [u8]) -> Result<usize, &'static str> {
    let mut lines = article
        .split(|&b| b == b'\n')
        .map(|l| l.strip_suffix(b"\r").unwrap_or(l));
    for line in lines.by_ref() {
        if line.starts_with(b"=ybegin ") {
            break;
        }
    }
    let mut written = 0;
    for line in lines {
        if line.starts_with(b"=yend") {
            break;
        }
        if line.starts_with(b"=ypart ") {
            continue;
        }
        let n = scalar_decode_line(line, &mut dst[written..])?;
        written += n;
    }
    Ok(written)
}

/// Pure-scalar reference line decoder: the vector fast path must agree with it
/// on every input, including lines that mix clean and escaped bytes.
fn scalar_decode_line(line: &[u8], dst: &mut [u8]) -> Result<usize, &'static str> {
    let mut i = 0;
    let mut o = 0;
    while i < line.len() {
        let mut b = line[i];
        i += 1;
        if b == b'=' {
            if i >= line.len() {
                return Err("escape at end");
            }
            b = line[i].wrapping_sub(64);
            i += 1;
        }
        if o >= dst.len() {
            return Err("overflow");
        }
        dst[o] = b.wrapping_sub(42);
        o += 1;
    }
    Ok(o)
}

fn check_yenc_roundtrip(raw: &[u8], seed_note: &str) {
    let mut article = Vec::new();
    encode_segment(&mut article, &SegmentParams::single("f.bin"), raw).expect("encode");
    let mut dst = vec![0u8; raw.len() + 64];
    let n = decode_to("fuzz", &article, &mut dst).expect("must decode");
    assert_eq!(
        &dst[..n],
        raw,
        "{seed_note}: kernel output differs from input"
    );

    let mut expect = vec![0u8; raw.len() + 64];
    let m = scalar_decode_article(&article, &mut expect).expect("oracle decodes");
    assert_eq!(
        &expect[..m],
        &dst[..n],
        "{seed_note}: vector path disagrees with scalar oracle"
    );
}

/// `#[test]` runs this natively (scalar backends); `#[wasm_bindgen_test]`
/// exports it to the wasm runner (vector backends on wasm32+simd128).
#[cfg_attr(not(target_arch = "wasm32"), test)]
#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test)]
fn yenc_simd128_agrees_with_scalar_oracle() {
    let mut rng = Rng(0x243f_6a88_85a3_08d3);
    for round in 0..200 {
        let len = (rng.next() % 300) as usize;
        let mut raw = vec![0u8; len];
        // Bias towards escapable outputs every few rounds so dirty lanes (and
        // clean/dirty lane transitions inside one line) are exercised.
        let bias = round % 4 == 0;
        for b in raw.iter_mut() {
            let v = rng.next() as u8;
            *b = if bias {
                [0xd6, 0xe3, 0xe8, 0x13, v][(rng.next() % 5) as usize]
            } else {
                v
            };
        }
        check_yenc_roundtrip(&raw, &format!("round {round}"));
    }
}

/// `#[test]` runs this natively (scalar backends); `#[wasm_bindgen_test]`
/// exports it to the wasm runner (vector backends on wasm32+simd128).
#[cfg_attr(not(target_arch = "wasm32"), test)]
#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test)]
fn yenc_simd128_clean_lanes_decode() {
    // `A`..`Z` encode (+42) to 107..132, none of which is NUL/LF/CR/`=`, so no
    // byte escapes: every 16-byte lane is clean and the whole body takes the
    // vector path on simd128 targets. 4096 bytes fill thirty-two 128-column
    // lines, so the lane loop runs repeatedly.
    let raw: Vec<u8> = (0..4096).map(|i| b'A' + (i % 26) as u8).collect();
    let mut article = Vec::new();
    encode_segment(&mut article, &SegmentParams::single("f.bin"), &raw).expect("encode");
    // Body lines sit between the `=ybegin` header and the `=yend` trailer
    // (matched explicitly: an encoded line may itself start with `=`).
    let lines: Vec<&[u8]> = article.split(|&b| b == b'\n').collect();
    let body = &lines[1..lines
        .iter()
        .position(|l| l.starts_with(b"=yend"))
        .expect("trailer")];
    let body_has_escape = body.iter().any(|l| l.contains(&b'='));
    assert!(
        !body_has_escape,
        "fixture premise broken: encoded body contains an escape"
    );
    check_yenc_roundtrip(&raw, "clean lanes");
}

// ---------------------------------------------------------------------------
// PAR2 gf16 madd
// ---------------------------------------------------------------------------

/// Independent field multiply straight from the polynomial
/// x^16 + x^12 + x^3 + x + 1 (reduction `0x100B`): any divergence from `madd`
/// (scalar or simd128 dispatch) is a kernel bug.
fn model_mul(mut a: u16, mut b: u16) -> u16 {
    let mut acc = 0u16;
    while b != 0 {
        if b & 1 != 0 {
            acc ^= a;
        }
        let carry = a & 0x8000 != 0;
        a <<= 1;
        if carry {
            a ^= 0x100B;
        }
        b >>= 1;
    }
    acc
}

fn check_gf16_madd(words: usize, factor: u16, src: &[u16], note: &str) {
    assert_eq!(src.len(), words);
    let mut want = vec![0x5a5au16; words];
    for (d, s) in want.iter_mut().zip(src.iter()) {
        *d ^= model_mul(*s, factor);
    }
    // Scalar backend directly.
    let t = gf16::nibble_tables(factor);
    let mut got_scalar = vec![0x5a5au16; words];
    gf16::madd_scalar(&mut got_scalar, src, &t);
    assert_eq!(got_scalar, want, "{note}: scalar backend wrong");
    // Dispatched path (simd128 where the target enables it): whole 8-word
    // chunks take the vector kernel, the tail the scalar loop.
    let mut got = vec![0x5a5au16; words];
    gf16::madd(&mut got, src, factor);
    assert_eq!(got, want, "{note}: dispatched backend wrong");
}

/// `#[test]` runs this natively (scalar backends); `#[wasm_bindgen_test]`
/// exports it to the wasm runner (vector backends on wasm32+simd128).
#[cfg_attr(not(target_arch = "wasm32"), test)]
#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test)]
fn gf16_madd_matches_double_and_add_oracle() {
    let mut rng = Rng(0x8bad_f00d_cafe_1234);
    // Explicit vector boundaries: empty, sub-chunk, exact 8-word chunks, and
    // non-multiple-of-8 tails (the tail takes the scalar loop).
    for &words in &[0usize, 1, 7, 8, 9, 15, 16, 17, 24, 100, 300] {
        for &factor in &[0u16, 1, 0x1234, 0xFFFF] {
            let src: Vec<u16> = (0..words).map(|_| rng.next() as u16).collect();
            check_gf16_madd(
                words,
                factor,
                &src,
                &format!("words={words} factor={factor:#06x}"),
            );
        }
    }
    // Random rounds spanning >=8-word inputs and odd tails.
    for round in 0..30 {
        let words = 1 + (rng.next() % 300) as usize;
        let factor = rng.next() as u16;
        let src: Vec<u16> = (0..words).map(|_| rng.next() as u16).collect();
        check_gf16_madd(words, factor, &src, &format!("round {round}"));
    }
    // Review-scale buffer: 360,512 words, one factor, prefilled destination
    // (proves XOR-accumulate, not overwrite, at scale).
    let big: Vec<u16> = (0..360_512).map(|_| rng.next() as u16).collect();
    check_gf16_madd(360_512, 0x1d3f, &big, "360,512-word buffer");
    // Zero factor is a no-op and empty slices are safe.
    let mut dst = vec![1u16, 2, 3];
    gf16::madd(&mut dst, &[4u16, 5, 6], 0);
    assert_eq!(dst, vec![1u16, 2, 3]);
    let mut empty: Vec<u16> = Vec::new();
    gf16::madd(&mut empty, &[], 7);
}
