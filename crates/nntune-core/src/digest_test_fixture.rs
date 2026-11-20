use crate::digest_enrollment::{DigestEnrollment, OriginManifest};
use crate::digest_geometry::DigestGeometry;
use crate::digest_page::{encode_page, page_hash};
use crate::media::{FileKey, Profile, Sealer};
use crate::rng::FillRandom;
use sha2::{Digest, Sha256};

pub(crate) fn id(n: u8) -> [u8; 16] {
    let mut id = [n; 16];
    id[6] = 0x40;
    id[8] = 0x80;
    id
}

pub(crate) struct Fixture {
    pub manifest: OriginManifest,
    pub key: FileKey,
    pub header: [u8; 48],
    pub chunks: Vec<Vec<u8>>,
    pub pages: Vec<Vec<u8>>,
}

pub(crate) fn fixture(plaintext: &[u8]) -> Fixture {
    fixture_with_identities(plaintext, [id(1); 5])
}

pub(crate) fn fixture_with_identities(plaintext: &[u8], identities: [[u8; 16]; 5]) -> Fixture {
    let mut builder = DigestEnrollment::new(plaintext.len() as u64, identities, id(2)).unwrap();
    let key = FileKey::from_bytes(*builder.key().expose_secret());
    let header = builder.header().to_bytes();
    let mut chunks = Vec::new();
    let mut pages = Vec::new();
    for part in plaintext.chunks(65_536) {
        let output = builder.seal_chunk(part).unwrap();
        chunks.push(output.bytes);
        if let Some(page) = output.page {
            pages.push(page);
        }
    }
    Fixture {
        manifest: builder.finish().ok().unwrap(),
        key,
        header,
        chunks,
        pages,
    }
}

struct Fixed;
impl FillRandom for Fixed {
    fn fill_bytes(&mut self, dest: &mut [u8]) -> crate::Result<()> {
        dest.fill(9);
        Ok(())
    }
}

/// Synthetic evidence for empty objects or deliberately incorrect finality.
/// It does not represent an enrolled origin and is only available to tests.
pub(crate) fn synthetic_single_chunk(length: u64, final_chunk: bool) -> Fixture {
    assert!(length <= 65_536);
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
    Fixture {
        manifest,
        key,
        header,
        chunks: vec![sealed],
        pages: vec![page],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::digest_chunk::open_chunk;
    use crate::digest_geometry::DigestGeometry;
    use sha2::{Digest, Sha256};

    #[test]
    fn fixture_enrolls_and_opens_across_page_boundary() {
        let mut plaintext = vec![42; 256 * 65_536 + 1];
        *plaintext.last_mut().unwrap() = 9;
        let fixture = fixture(&plaintext);
        let geometry = DigestGeometry::new(plaintext.len() as u64).unwrap();
        assert_eq!(fixture.chunks.len(), geometry.chunks() as usize);
        assert_eq!(fixture.pages.len(), geometry.pages() as usize);
        for index in [0, 255, 256] {
            let page = &fixture.pages[index / 256];
            let entry = 92 + (index % 256) * 32;
            assert_eq!(
                &page[entry..entry + 32],
                Sha256::digest(&fixture.chunks[index]).as_slice()
            );
            let opened = open_chunk(
                &fixture.manifest,
                &fixture.key,
                index as u32,
                page,
                fixture.chunks[index].clone(),
                (index == 0).then_some(fixture.header.as_slice()),
            )
            .unwrap();
            let start = index * 65_536;
            let end = (start + 65_536).min(plaintext.len());
            assert_eq!(&**opened, &plaintext[start..end]);
        }
    }
}
