//! Exercise the crate-private reader with complete synthetic frames.
#![allow(dead_code)]

#[path = "../src/flac_frame.rs"]
mod flac_frame;
#[path = "../src/flac_frame_footer.rs"]
mod flac_frame_footer;
#[path = "../src/flac_frame_header.rs"]
mod flac_frame_header;
#[path = "../src/flac_preamble.rs"]
mod flac_preamble;
#[path = "../src/flac_predictor.rs"]
mod flac_predictor;
#[path = "../src/flac_residual.rs"]
mod flac_residual;
#[path = "../src/flac_samples.rs"]
mod flac_samples;
#[path = "../src/flac_stereo.rs"]
mod flac_stereo;
#[path = "../src/flac_subframe.rs"]
mod flac_subframe;

use flac_frame::FrameReader;
use flac_preamble::StreamInfo;

const INFO: StreamInfo = StreamInfo {
    min_block: 16,
    max_block: 16,
    sample_rate: 44_100,
    channels: 1,
    bits_per_sample: 4,
    total_samples: 16,
    pcm_md5: [0; 16],
};

fn crc(bytes: &[u8], width: u8, polynomial: u32) -> u16 {
    let mut remainder = 0;
    for &byte in bytes {
        for bit in (0..8).rev() {
            remainder = (remainder << 1) | u32::from((byte >> bit) & 1);
            if remainder & (1 << width) != 0 {
                remainder ^= polynomial;
            }
        }
    }
    for _ in 0..width {
        remainder <<= 1;
        if remainder & (1 << width) != 0 {
            remainder ^= polynomial;
        }
    }
    remainder as u16
}

fn frame(format: u8, body: &[u8]) -> Vec<u8> {
    let mut bytes = vec![0xff, 0xf8, 0x69, format, 0, 15];
    bytes.push(crc(&bytes, 8, 0x107) as u8);
    bytes.extend_from_slice(body);
    bytes.extend_from_slice(&crc(&bytes, 16, 0x18005).to_be_bytes());
    bytes
}

fn packed(fields: &[(u8, u32)]) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut bits = 0_usize;
    for &(width, value) in fields {
        for bit in (0..width).rev() {
            if bits.is_multiple_of(8) {
                bytes.push(0);
            }
            *bytes.last_mut().unwrap() |= (((value >> bit) & 1) as u8) << (7 - bits % 8);
            bits += 1;
        }
    }
    bytes
}

#[test]
fn rejects_frame_format_mismatch_before_consuming_header_or_scratch() {
    let wire = frame(2, &[0, 42]);
    let matching = StreamInfo {
        bits_per_sample: 8,
        ..INFO
    };
    let mut input = wire.as_slice();
    let mut reader = FrameReader::start(&mut input, &matching).unwrap().unwrap();
    assert_eq!(reader.read(&mut input), Ok(true));
    assert!(reader.finish().is_ok());

    for info in [
        INFO,
        StreamInfo {
            channels: 2,
            ..matching
        },
        StreamInfo {
            sample_rate: 48_000,
            ..matching
        },
    ] {
        let mut input = wire.as_slice();
        let mut scratch = Some(vec![42]);
        assert!(FrameReader::start_with_pcm(&mut input, &info, &mut scratch).is_err());
        assert_eq!(input, wire);
        assert_eq!(scratch, Some(vec![42]));
    }
}

#[test]
fn rejects_impossible_rice_prefix_without_waiting_for_a_terminator() {
    // Order zero at four bits can only produce folded values 0 through 15.
    // Method, partition order and parameter occupy ten bits before the zeros.
    let wire = frame(0, &[0x10]);
    let mut input = &wire[..8];
    let mut reader = FrameReader::start(&mut input, &INFO).unwrap().unwrap();
    assert_eq!(reader.read(&mut input), Ok(false));
    assert_eq!(reader.read(&mut [0; 3].as_slice()), Ok(false)); // 14 unary zeros.
    let mut rest = [0, 0xff, 0xf8].as_slice();
    assert_eq!(reader.read(&mut rest), Err(()));
    assert_eq!(rest, [0xff, 0xf8]);
    assert_eq!(reader.read(&mut rest), Err(()));
    assert_eq!(rest, [0xff, 0xf8]);
    assert!(reader.finish().is_err());
}

#[test]
fn predictive_limits_accept_extrema_at_every_split_and_preserve_the_next_frame() {
    let mut fixed_zero = vec![(8, 0x10), (6, 0), (4, 2), (6, 7)]; // Rice(-8), k=2.
    fixed_zero.extend(std::iter::repeat_n((3, 4), 15)); // Rice(0), k=2.
    let mut fixed_one = vec![(8, 0x12), (4, 8), (6, 0), (4, 0), (31, 1)]; // -8 + 15 = 7.
    fixed_one.extend(std::iter::repeat_n((1, 1), 14));
    // LPC order 1, precision 1, shift 0, coefficient -1. Each prediction
    // is +8 and each residual is -16 (31 zero bits and the terminator).
    let mut lpc = vec![(8, 0x40), (4, 8), (9, 0), (1, 1), (6, 0), (4, 0)];
    lpc.extend(std::iter::repeat_n((32, 1), 15));
    for (fields, following) in [(fixed_zero, 0), (fixed_one, 7), (lpc, -8)] {
        let wire = frame(0, &packed(&fields));
        let joined = [wire.as_slice(), wire.as_slice()].concat();
        let mut expected = vec![following; 16];
        expected[0] = -8;
        for split in 7..wire.len() {
            let (mut prefix, mut rest) = joined.split_at(split);
            let mut scratch = Some(Vec::new());
            let mut reader = FrameReader::start_with_pcm(&mut prefix, &INFO, &mut scratch)
                .unwrap()
                .unwrap();
            assert_eq!(reader.read(&mut prefix), Ok(false), "split {split}");
            assert!(prefix.is_empty());
            assert_eq!(reader.read(&mut rest), Ok(true), "split {split}");
            assert_eq!(rest, wire);
            assert_eq!(reader.finish_with_pcm().unwrap().1.unwrap(), expected);
        }
    }
}
