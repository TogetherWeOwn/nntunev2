use super::*;
use crate::digest_enrollment::DigestEnrollment;
use crate::digest_geometry::DigestGeometry;
use crate::digest_page::{encode_page, page_hash};
use crate::media::Sealer;
use crate::rng::FillRandom;

fn id(n: u8) -> [u8; 16] {
    let mut id = [n; 16];
    id[6] = 0x40;
    id[8] = 0x80;
    id
}

#[test]
fn opens_chunks_and_rejects_untrusted_inputs() {
    let mut builder = DigestEnrollment::new(65_537, [id(1); 5], id(2)).unwrap();
    let key = FileKey::from_bytes(*builder.key().expose_secret());
    let header = builder.header().to_bytes();
    let first = builder.seal_chunk(&vec![7; 65_536]).unwrap().bytes;
    let last = builder.seal_chunk(&[9]).unwrap();
    let page = last.page.unwrap();
    let manifest = builder.finish().ok().unwrap();
    assert_eq!(
        &**open_chunk(&manifest, &key, 0, &page, first.clone(), Some(&header)).unwrap(),
        &vec![7; 65_536]
    );
    assert_eq!(
        &**open_chunk(&manifest, &key, 1, &page, last.bytes.clone(), None).unwrap(),
        &[9]
    );
    let mut changed = last.bytes.clone();
    changed[0] ^= 1;
    assert!(matches!(
        open_chunk(&manifest, &key, 1, &page, changed, None),
        Err(ChunkError::DigestMismatch)
    ));
    assert!(matches!(
        open_chunk(&manifest, &key, 1, &page, vec![0; 16], None),
        Err(ChunkError::InvalidChunk)
    ));
    assert!(open_chunk(&manifest, &key, 2, &page, last.bytes.clone(), None).is_err());
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
            last.bytes,
            None
        ),
        Err(ChunkError::Header)
    ));
}

struct Fixed;
impl FillRandom for Fixed {
    fn fill_bytes(&mut self, dest: &mut [u8]) -> crate::Result<()> {
        dest.fill(9);
        Ok(())
    }
}

#[test]
fn empty_final_is_authenticated_and_wrong_finality_fails() {
    for (length, final_chunk) in [(0_u64, true), (65_536, false)] {
        let key = FileKey::from_bytes([5; 32]);
        let mut sealer = Sealer::random(&Profile::nntune(), &key, &mut Fixed).unwrap();
        let header = sealer.header().to_bytes();
        let sealed = sealer
            .seal_chunk(&vec![7; length as usize], final_chunk)
            .unwrap();
        let geometry = DigestGeometry::new(length).unwrap();
        let map = id(2);
        let context = [3; 32];
        let digest = <[u8; 32]>::from(Sha256::digest(&sealed));
        let page = encode_page(geometry, &map, &context, 0, &[digest]).unwrap();
        let mut manifest = OriginManifest::synthetic_test_manifest(
            geometry,
            map,
            context,
            vec![page_hash(&page).unwrap()],
        );
        manifest.set_test_header(header);
        let result = open_chunk(&manifest, &key, 0, &page, sealed, Some(&header));
        if final_chunk {
            assert!(result.unwrap().is_empty());
        } else {
            assert!(matches!(result, Err(ChunkError::Authentication)));
        }
    }
}
