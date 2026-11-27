use nntune_core::{
    digest_geometry::DigestGeometry,
    media::{ciphertext_size, HEADER_SIZE, PLAIN_CHUNK_SIZE},
    media_layout::plan_articles,
    origin_sealer::MAX_NEW_PLAINTEXT,
};

#[test]
fn digest_chunks_and_articles_partition_the_codec_extent() {
    for size in [
        0,
        1,
        PLAIN_CHUNK_SIZE,
        PLAIN_CHUNK_SIZE * 256 + 1,
        MAX_NEW_PLAINTEXT,
    ] {
        let geometry = DigestGeometry::new(size).unwrap();
        let range = geometry.range(0, size).unwrap();
        let total = ciphertext_size(size).unwrap();
        assert_eq!(range.ciphertext_span(), Some(HEADER_SIZE..total));
        let mut index = 0;
        for article in plan_articles(size, 11).unwrap() {
            let mut offset = article.ciphertext_offset;
            let end = offset + article.ciphertext_len;
            while offset < end {
                let chunk = range.sealed_chunk(index).unwrap();
                assert_eq!(chunk.ciphertext.start, offset);
                assert_eq!(chunk.final_chunk, index + 1 == geometry.chunks());
                offset = chunk.ciphertext.end;
                index += 1;
            }
            assert_eq!(offset, end);
        }
        assert_eq!(index, geometry.chunks());
        assert!(range.sealed_chunk(index).is_none());
    }
    assert!(DigestGeometry::new(MAX_NEW_PLAINTEXT + 1).is_none());
    assert!(plan_articles(MAX_NEW_PLAINTEXT + 1, 11).is_some());
}
