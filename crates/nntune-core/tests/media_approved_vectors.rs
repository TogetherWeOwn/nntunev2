//! Synthetic Random/NNTune vectors from the independently generated corpus at
//! velmora pin a0a9694dca3244f4bdd2e890395d3735a198ce90. The source corpus
//! SHA-256 is eb34d6920bf094f36d07c2e147f776c489cbd9a5f1b19d976d64daf46c07443b.
use nntune_core::{
    media::{ContentIdentity, FileKey, KeyMode, Profile, Sealer, VerifiedHeader},
    rng::FillRandom,
    Result,
};

struct VectorNonce;

impl FillRandom for VectorNonce {
    fn fill_bytes(&mut self, dest: &mut [u8]) -> Result<()> {
        assert_eq!(dest.len(), 16);
        for (i, byte) in dest.iter_mut().enumerate() {
            *byte = 0xa0 + i as u8;
        }
        Ok(())
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[test]
fn approved_random_media_vectors_match_and_open() {
    let sizes = [0, 1, 65_535, 65_536, 65_537, 720_896, 720_897, 196_608];
    let wire_lengths = [64, 65, 65_599, 65_600, 65_617, 721_120, 721_137, 196_704];
    let digests = [
        "1a7eb709e02837426a42a19f69dbd6e49d0563d1ea1fc1b7179a0b1b8a56e5c2",
        "2b09c284bfecae4a7215a15956e3b27ceaf06baa660700875f055e6d35c5ae15",
        "b245b8a4c4d7fb93623d777ce973248af1d502dbf509b53da3b9195dc4da6718",
        "bf0a49ef6e816a009b7e25f7efd457eb88163e0b38b9ba1095d0bb65c1529518",
        "41077c1a9caee69eb720a4dce47d9c8acb288b51deb2eb876cec5b760a1ccba9",
        "9aea24e0f26a1c3bac85713ddcffd647bd05171b04b163c25fb9ef1acc2e15d3",
        "ae1a059cbfcd71d00b87d2781406939be8731bbbff2268d35490d04292dd8bc7",
        "3fce1d060ea89435ebc43f418a270f434f1eddba93a4cca281adbb14bff0f8f4",
    ];
    let key = FileKey::from_bytes(std::array::from_fn(|i| i as u8));
    for ((size, wire_len), digest) in sizes.into_iter().zip(wire_lengths).zip(digests) {
        let plain: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
        let sealer = Sealer::random(&Profile::nntune(), &key, &mut VectorNonce).unwrap();
        let header = sealer.header();
        let wire = sealer.seal_to_vec(&plain).unwrap();
        assert_eq!(wire.len(), wire_len, "size={size}");
        assert_eq!(
            hex(ContentIdentity::of(&wire).digest()),
            digest,
            "size={size}"
        );
        let verified =
            VerifiedHeader::verify(&Profile::nntune(), KeyMode::Random, &key, &header).unwrap();
        let reader = verified.reader(size as u64).unwrap();
        assert_eq!(
            reader.verify_all(&wire).unwrap(),
            *ContentIdentity::of(&plain).digest()
        );
    }
}
