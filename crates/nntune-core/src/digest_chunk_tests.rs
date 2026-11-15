use super::*;
use crate::digest_test_fixture::{fixture, synthetic_single_chunk, Fixture};

#[test]
fn opens_chunks_and_rejects_untrusted_inputs() {
    let mut plaintext = vec![7; 65_537];
    plaintext[65_536] = 9;
    let fixture = fixture(&plaintext);
    let manifest = fixture.manifest;
    let key = fixture.key;
    let header = fixture.header;
    let first = fixture.chunks[0].clone();
    let last = fixture.chunks[1].clone();
    let page = fixture.pages[0].clone();
    assert_eq!(
        &**open_chunk(&manifest, &key, 0, &page, first.clone(), Some(&header)).unwrap(),
        &vec![7; 65_536]
    );
    assert_eq!(
        &**open_chunk(&manifest, &key, 1, &page, last.clone(), None).unwrap(),
        &[9]
    );
    let mut changed = last.clone();
    changed[0] ^= 1;
    assert!(matches!(
        open_chunk(&manifest, &key, 1, &page, changed, None),
        Err(ChunkError::DigestMismatch)
    ));
    assert!(matches!(
        open_chunk(&manifest, &key, 1, &page, vec![0; 16], None),
        Err(ChunkError::InvalidChunk)
    ));
    assert!(open_chunk(&manifest, &key, 2, &page, last.clone(), None).is_err());
    let mut bad = page.clone();
    bad[92] ^= 1;
    assert!(matches!(
        open_chunk(&manifest, &key, 0, &bad, first.clone(), Some(&header)),
        Err(ChunkError::Page(_))
    ));
    let mut bad_header = header;
    bad_header[0] ^= 1;
    assert!(matches!(
        open_chunk(&manifest, &key, 0, &page, first.clone(), Some(&bad_header)),
        Err(ChunkError::Header)
    ));
    assert!(matches!(
        open_chunk(&manifest, &key, 0, &page, first, None),
        Err(ChunkError::Header)
    ));
    assert!(matches!(
        open_chunk(
            &manifest,
            &FileKey::from_bytes([0; 32]),
            1,
            &page,
            last,
            None
        ),
        Err(ChunkError::Header)
    ));
}

#[test]
fn opens_only_selected_authenticated_bytes() {
    let mut plaintext = vec![7; 65_537];
    plaintext[65_536] = 9;
    let f = fixture(&plaintext);
    for (index, expected) in [(0, 7), (1, 9)] {
        let header = if index == 0 {
            Some(&f.header[..])
        } else {
            None
        };
        let part = open_chunk_part(
            &f.manifest,
            &f.key,
            65_535..65_537,
            index,
            &f.pages[0],
            f.chunks[index as usize].clone(),
            header,
        )
        .unwrap();
        assert_eq!(part.as_slice(), &[expected]);
    }
    let mut changed = f.chunks[0].clone();
    changed[0] ^= 1;
    assert!(matches!(
        open_chunk_part(
            &f.manifest,
            &f.key,
            65_535..65_537,
            0,
            &f.pages[0],
            changed,
            Some(&f.header)
        ),
        Err(ChunkError::DigestMismatch)
    ));
    assert!(matches!(
        open_chunk_part(
            &f.manifest,
            &f.key,
            0..1,
            1,
            &f.pages[0],
            f.chunks[1].clone(),
            None
        ),
        Err(ChunkError::InvalidChunk)
    ));
    assert!(matches!(
        open_chunk_part(
            &f.manifest,
            &f.key,
            0..65_538,
            0,
            &f.pages[0],
            f.chunks[0].clone(),
            Some(&f.header)
        ),
        Err(ChunkError::InvalidChunk)
    ));
}

#[test]
fn empty_final_is_authenticated_and_wrong_finality_fails() {
    for (length, final_chunk) in [(0_u64, true), (65_536, false)] {
        let Fixture {
            manifest,
            key,
            header,
            chunks,
            pages,
        } = synthetic_single_chunk(length, final_chunk);
        let sealed = chunks[0].clone();
        let page = &pages[0];
        let part = open_chunk_part(
            &manifest,
            &key,
            0..length,
            0,
            page,
            sealed.clone(),
            Some(&header),
        );
        let result = open_chunk(&manifest, &key, 0, page, sealed, Some(&header));
        if final_chunk {
            assert!(result.unwrap().is_empty());
            assert!(part.unwrap().as_slice().is_empty());
        } else {
            assert!(matches!(result, Err(ChunkError::Authentication)));
            assert!(matches!(part, Err(ChunkError::Authentication)));
        }
    }
}
