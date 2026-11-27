use super::*;

#[test]
fn completed_frames_must_match_the_stream_pcm_format() {
    // A complete 12-bit mono frame and a complete 12-bit stereo frame,
    // each with 192 samples per channel at 44.1 kHz.
    let mono = [0xff, 0xf8, 0x19, 4, 0, 0x46, 0, 0x5a, 0xb0, 0x7f, 0x33];
    let stereo = [
        0xff, 0xf8, 0x19, 0x14, 0, 0x11, 0, 0x5a, 0xb0, 0x0a, 0x34, 0x56, 0x21,
    ];
    let info = StreamInfo {
        min_block: 192,
        max_block: 192,
        sample_rate: 44_100,
        channels: 1,
        bits_per_sample: 12,
        total_samples: 192,
        pcm_md5: [0; 16],
    };
    let read = |mut input: &[u8], format: &StreamInfo| {
        let mut frame = FrameReader::start(&mut input, format).unwrap().unwrap();
        assert_eq!(frame.read(&mut input), Ok(true));
        assert!(input.is_empty());
        frame
    };
    let mut valid = FrameContinuity::new(&info);
    assert_eq!(valid.accept(read(&mono, &info)), Ok(()));
    assert_eq!(valid.finish_at_frame_boundary(), Ok(()));

    let cases = [
        (
            read(&mono, &info),
            StreamInfo {
                sample_rate: 48_000,
                ..info
            },
        ),
        (
            read(&mono, &info),
            StreamInfo {
                bits_per_sample: 16,
                ..info
            },
        ),
        (
            read(
                &stereo,
                &StreamInfo {
                    channels: 2,
                    ..info
                },
            ),
            info,
        ),
    ];
    for (frame, expected) in cases {
        let mut invalid = FrameContinuity::new(&expected);
        assert_eq!(invalid.accept(frame), Err(()));
        assert_eq!(invalid.accept(read(&mono, &info)), Err(()));
        assert_eq!(invalid.finish_at_frame_boundary(), Err(()));
    }
}
