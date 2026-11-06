//! Keyless, bounded preparation input, not a posting manifest or POST authority.
//! Format and private service frames are fixed binary sequences. No general
//! object decoder, unbounded count allocation, filenames or article identities.

use crate::{decrypt::rclone::PLAIN_BLOCK_SIZE, encrypt};

/// Opened preparation buffers wipe on drop, including cancellation paths.
pub type PlanBytes = zeroize::Zeroizing<Vec<u8>>;

pub const PLAN_BYTE_CAP: usize = 1_048_576;
const MAGIC: &[u8; 8] = b"NNPREP01";
const SPAN_BYTES: usize = 32;
const MAX_EXACT_INTEGER: u64 = (1_u64 << 53) - 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidPlan;

/// Explicit environment policy only. Requests and Queue bodies cannot enable it.
pub fn preparation_enabled(value: Option<&str>) -> Result<bool, InvalidPlan> {
    match value {
        None | Some("disabled") => Ok(false),
        Some("preparation_only") => Ok(true),
        _ => Err(InvalidPlan),
    }
}

/// No Debug/serde: even keyless opened preparation metadata is not log material.
#[derive(Clone, PartialEq, Eq)]
pub struct PreparationPlan {
    pub owner: String,
    pub job: String,
    pub operation: String,
    pub predecessor: u64,
    pub original_digest: [u8; 32],
    pub original_bytes: u64,
    pub file_count: u32,
    pub file_index: u32,
    pub object: String,
    pub object_version: String,
    pub plaintext_bytes: u64,
    pub plaintext_digest: [u8; 32],
    pub blocks_per_article: u32,
}

impl PreparationPlan {
    fn spans(&self) -> Result<Vec<encrypt::ArticleSpan>, InvalidPlan> {
        if self.predecessor >= i64::MAX as u64
            || self.original_bytes == 0
            || self.original_bytes > i64::MAX as u64
            || self.file_count == 0
            || self.file_count > i32::MAX as u32
            || self.file_index >= self.file_count
            || self.plaintext_bytes == 0
            || self.plaintext_bytes > MAX_EXACT_INTEGER
            || self.blocks_per_article == 0
        {
            return Err(InvalidPlan);
        }
        // Bound BEFORE calling the existing allocating geometry helper.
        let count = self
            .plaintext_bytes
            .div_ceil(PLAIN_BLOCK_SIZE)
            .div_ceil(u64::from(self.blocks_per_article));
        if count > (PLAN_BYTE_CAP / SPAN_BYTES) as u64 {
            return Err(InvalidPlan);
        }
        encrypt::plan_articles(self.plaintext_bytes, u64::from(self.blocks_per_article))
            .ok_or(InvalidPlan)
    }

    pub fn encode(&self) -> Result<Vec<u8>, InvalidPlan> {
        let spans = self.spans()?;
        let mut out = MAGIC.to_vec();
        for value in [&self.owner, &self.job, &self.operation] {
            put_atom(&mut out, value)?;
        }
        out.push(1);
        out.extend(self.predecessor.to_be_bytes());
        out.extend(self.original_digest);
        out.extend(self.original_bytes.to_be_bytes());
        out.extend(self.file_count.to_be_bytes());
        out.extend(self.file_index.to_be_bytes());
        put_atom(&mut out, &self.object)?;
        put_atom(&mut out, &self.object_version)?;
        out.extend(self.plaintext_bytes.to_be_bytes());
        out.extend(self.plaintext_digest);
        out.push(1);
        out.extend(self.blocks_per_article.to_be_bytes());
        out.extend(
            encrypt::ciphertext_len(self.plaintext_bytes)
                .ok_or(InvalidPlan)?
                .to_be_bytes(),
        );
        out.extend((spans.len() as u32).to_be_bytes());
        if out
            .len()
            .checked_add(spans.len() * SPAN_BYTES)
            .ok_or(InvalidPlan)?
            > PLAN_BYTE_CAP
        {
            return Err(InvalidPlan);
        }
        for span in spans {
            for n in [
                span.plaintext_start,
                span.plaintext_end,
                span.ciphertext_offset,
                span.ciphertext_len,
            ] {
                out.extend(n.to_be_bytes());
            }
        }
        Ok(out)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, InvalidPlan> {
        if bytes.len() > PLAN_BYTE_CAP {
            return Err(InvalidPlan);
        }
        let mut input = Input(bytes);
        if input.take::<8>()? != *MAGIC {
            return Err(InvalidPlan);
        }
        let owner = input.atom()?;
        let job = input.atom()?;
        let operation = input.atom()?;
        if input.take::<1>()? != [1] {
            return Err(InvalidPlan);
        }
        let predecessor = input.u64()?;
        let original_digest = input.take()?;
        let original_bytes = input.u64()?;
        let file_count = input.u32()?;
        let file_index = input.u32()?;
        let object = input.atom()?;
        let object_version = input.atom()?;
        let plaintext_bytes = input.u64()?;
        let plaintext_digest = input.take()?;
        if input.take::<1>()? != [1] {
            return Err(InvalidPlan);
        }
        let blocks_per_article = input.u32()?;
        let cipher_bytes = input.u64()?;
        let count = input.u32()? as usize;
        if count.checked_mul(SPAN_BYTES) != Some(input.0.len()) {
            return Err(InvalidPlan);
        }
        let plan = Self {
            owner,
            job,
            operation,
            predecessor,
            original_digest,
            original_bytes,
            file_count,
            file_index,
            object,
            object_version,
            plaintext_bytes,
            plaintext_digest,
            blocks_per_article,
        };
        let spans = plan.spans()?;
        if count != spans.len() || Some(cipher_bytes) != encrypt::ciphertext_len(plaintext_bytes) {
            return Err(InvalidPlan);
        }
        for span in spans {
            for expected in [
                span.plaintext_start,
                span.plaintext_end,
                span.ciphertext_offset,
                span.ciphertext_len,
            ] {
                if input.u64()? != expected {
                    return Err(InvalidPlan);
                }
            }
        }
        input.end()?;
        Ok(plan)
    }
}

pub const FREEZE: u8 = 1;
pub const OPEN: u8 = 2;

/// Exact plan reference; no caller owner, latest selector or URL enters open.
#[derive(Clone, PartialEq, Eq)]
pub struct PlanReference {
    pub id: String,
    pub version: String,
    pub digest: [u8; 32],
    pub bytes: u32,
}

impl PlanReference {
    pub fn encode(&self) -> Result<Vec<u8>, InvalidPlan> {
        let mut out = vec![1];
        put_atom(&mut out, &self.id)?;
        put_atom(&mut out, &self.version)?;
        if self.bytes == 0 || self.bytes as usize > PLAN_BYTE_CAP {
            return Err(InvalidPlan);
        }
        out.extend(self.digest);
        out.extend(self.bytes.to_be_bytes());
        Ok(out)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, InvalidPlan> {
        let mut input = Input(bytes);
        if input.take::<1>()? != [1] {
            return Err(InvalidPlan);
        }
        let value = Self {
            id: input.atom()?,
            version: input.atom()?,
            digest: input.take()?,
            bytes: input.u32()?,
        };
        input.end()?;
        value.encode()?;
        Ok(value)
    }

    pub fn open_request(&self, dispatch: &str) -> Result<Vec<u8>, InvalidPlan> {
        let mut out = vec![OPEN];
        put_atom(&mut out, dispatch)?;
        out.extend(self.encode()?);
        Ok(out)
    }

    pub fn decode_open(bytes: &[u8]) -> Result<(String, Self), InvalidPlan> {
        let mut input = Input(bytes);
        if input.take::<1>()? != [OPEN] {
            return Err(InvalidPlan);
        }
        let dispatch = input.atom()?;
        Ok((dispatch, Self::decode(input.0)?))
    }
}

fn put_atom(out: &mut Vec<u8>, value: &str) -> Result<(), InvalidPlan> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(InvalidPlan);
    }
    out.push(value.len() as u8);
    out.extend(value.as_bytes());
    Ok(())
}

struct Input<'a>(&'a [u8]);
impl Input<'_> {
    fn take<const N: usize>(&mut self) -> Result<[u8; N], InvalidPlan> {
        let part = self
            .0
            .get(..N)
            .ok_or(InvalidPlan)?
            .try_into()
            .map_err(|_| InvalidPlan)?;
        self.0 = &self.0[N..];
        Ok(part)
    }
    fn u32(&mut self) -> Result<u32, InvalidPlan> {
        Ok(u32::from_be_bytes(self.take()?))
    }
    fn u64(&mut self) -> Result<u64, InvalidPlan> {
        Ok(u64::from_be_bytes(self.take()?))
    }
    fn atom(&mut self) -> Result<String, InvalidPlan> {
        let n = self.take::<1>()?[0] as usize;
        let value =
            std::str::from_utf8(self.0.get(..n).ok_or(InvalidPlan)?).map_err(|_| InvalidPlan)?;
        // Validate before allocating the bounded atom.
        if n == 0
            || n > 64
            || !value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        {
            return Err(InvalidPlan);
        }
        self.0 = &self.0[n..];
        Ok(value.to_owned())
    }
    fn end(&self) -> Result<(), InvalidPlan> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(InvalidPlan)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn plan() -> PreparationPlan {
        PreparationPlan {
            owner: "owner".into(),
            job: "job".into(),
            operation: "operation".into(),
            predecessor: 0,
            original_digest: [0; 32],
            original_bytes: 1,
            file_count: 1,
            file_index: 0,
            object: "object".into(),
            object_version: "version".into(),
            plaintext_bytes: 65537,
            plaintext_digest: [1; 32],
            blocks_per_article: 1,
        }
    }
    #[test]
    fn canonical_layout_and_short_tail() {
        let bytes = plan().encode().unwrap();
        assert_eq!(&bytes[..8], b"NNPREP01");
        assert!(PreparationPlan::decode(&bytes).unwrap() == plan());
        assert_eq!(
            PreparationPlan::decode(&bytes).unwrap().encode().unwrap(),
            bytes
        );
        let tail = &bytes[bytes.len() - 32..];
        assert_eq!(u64::from_be_bytes(tail[24..].try_into().unwrap()), 17);
        assert_eq!(
            u64::from_be_bytes(
                bytes[bytes.len() - 48..bytes.len() - 40]
                    .try_into()
                    .unwrap()
            ),
            32
        );
    }
    #[test]
    fn every_truncation_trailer_and_span_mutation_refuses() {
        let bytes = plan().encode().unwrap();
        for len in 0..bytes.len() {
            assert!(PreparationPlan::decode(&bytes[..len]).is_err())
        }
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(PreparationPlan::decode(&trailing).is_err());
        for index in bytes.len() - 64..bytes.len() {
            let mut changed = bytes.clone();
            changed[index] ^= 1;
            assert!(PreparationPlan::decode(&changed).is_err());
        }
    }
    #[test]
    fn geometry_is_bounded_before_allocation() {
        let mut p = plan();
        p.plaintext_bytes = MAX_EXACT_INTEGER;
        assert!(p.encode().is_err());
        p = plan();
        p.blocks_per_article = 0;
        assert!(p.encode().is_err());
        p = plan();
        p.predecessor = i64::MAX as u64;
        assert!(p.encode().is_err());
        p = plan();
        p.owner = "/".into();
        assert!(p.encode().is_err());
    }
    #[test]
    fn private_frames_are_exact_and_bounded() {
        let r = PlanReference {
            id: "plan".into(),
            version: "version".into(),
            digest: [0; 32],
            bytes: 1,
        };
        let frame = r.open_request("dispatch").unwrap();
        let (id, decoded) = PlanReference::decode_open(&frame).unwrap();
        assert_eq!(id, "dispatch");
        assert!(decoded == r);
        let mut extra = frame;
        extra.push(0);
        assert!(PlanReference::decode_open(&extra).is_err());
        assert!(!preparation_enabled(None).unwrap());
        assert!(preparation_enabled(Some("preparation_only")).unwrap());
        assert!(preparation_enabled(Some("true")).is_err());
    }
}
