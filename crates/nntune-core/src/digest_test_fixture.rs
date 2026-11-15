use crate::digest_enrollment::{DigestEnrollment, OriginManifest};
use crate::media::FileKey;

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
    let mut builder = DigestEnrollment::new(plaintext.len() as u64, [id(1); 5], id(2)).unwrap();
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
