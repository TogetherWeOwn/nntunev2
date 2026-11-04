//! Bounded original-media reader. Transport and cancellation/deadlines belong
//! to the request authority; this seam neither dials nor authenticates users.
//! The verified prefix walk is deliberately the baseline even with a hint.

use std::{
    future::{poll_fn, Future},
    io::Read,
    ops::Range,
    pin::pin,
    task::{Context, Poll},
};

use super::{OriginalSource, RangeHint, SourceError, MAX_ARTICLE_BYTES};
use crate::yenc::{self, DecodedPart};

/// Open exactly one private reference. Implementations must enforce their
/// own operation deadline, cancellation and bounded wire framing as well.
pub trait ArticleSource {
    type Body: Read;
    fn open(&mut self, reference: &str) -> Result<Self::Body, SourceError>;
}

pub trait ReadControl {
    /// Called before I/O, during collection and before releasing output.
    /// The authority must also cancel a blocked transport read itself.
    fn check(&mut self) -> Result<(), SourceError>;
}

struct Part {
    bytes: Vec<u8>,
    geometry: DecodedPart,
}

fn fetch<T: ArticleSource, C: ReadControl>(
    source: &OriginalSource,
    index: usize,
    decoded_budget: u64,
    transport: &mut T,
    control: &mut C,
) -> Result<Part, SourceError> {
    control.check()?;
    if decoded_budget == 0 {
        return Err(SourceError::Integrity);
    }
    let article = source.article(index)?;
    let mut body = transport.open(article.reference())?;
    let mut encoded = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        control.check()?;
        let n = body.read(&mut chunk).map_err(|_| SourceError::Transport)?;
        if n > chunk.len() || n > MAX_ARTICLE_BYTES - encoded.len() {
            return Err(SourceError::Bounds);
        }
        if n == 0 {
            break;
        }
        encoded.extend_from_slice(&chunk[..n]);
    }
    control.check()?;
    decode(source, index, decoded_budget, &encoded)
}

fn decode(
    source: &OriginalSource,
    index: usize,
    decoded_budget: u64,
    encoded: &[u8],
) -> Result<Part, SourceError> {
    if encoded.len() > MAX_ARTICLE_BYTES {
        return Err(SourceError::Bounds);
    }
    if decoded_budget == 0 {
        return Err(SourceError::Integrity);
    }
    // At most one decoded article, never the yEnc whole-file size. The
    // decoder checks capacity on every write as well as declared lengths.
    let decode_cap = decoded_budget.min(MAX_ARTICLE_BYTES as u64) as usize;
    let mut decoded = vec![0u8; encoded.len().min(decode_cap)];
    let geometry = yenc::decode_part_to("original-source", encoded, &mut decoded)
        .map_err(|_| SourceError::Integrity)?;
    decoded.truncate(geometry.written);
    if geometry.written == 0
        || geometry.file_size != source.file_size()
        || geometry.multipart != (source.article_count() > 1)
        || (geometry.multipart && geometry.part_number != Some(index as u64 + 1))
    {
        return Err(SourceError::Integrity);
    }
    let end = geometry
        .offset
        .checked_add(geometry.written as u64)
        .ok_or(SourceError::Integrity)?;
    if end > source.file_size()
        || (index + 1 == source.article_count() && end != source.file_size())
    {
        return Err(SourceError::Integrity);
    }
    Ok(Part {
        bytes: decoded,
        geometry,
    })
}

/// Half-open uniform candidates, usable only after actual endpoint proof.
#[derive(Clone, Copy, Debug)]
pub struct UniformRanges {
    part_size: u64,
    file_size: u64,
    count: usize,
}

impl UniformRanges {
    pub fn candidate(&self, index: usize) -> Result<Range<u64>, SourceError> {
        if index >= self.count {
            return Err(SourceError::Bounds);
        }
        let start = (index as u64)
            .checked_mul(self.part_size)
            .ok_or(SourceError::Bounds)?;
        let end = start
            .checked_add(self.part_size)
            .ok_or(SourceError::Bounds)?
            .min(self.file_size);
        Ok(start..end)
    }

    pub fn corroborates(&self, index: usize, actual: &DecodedPart) -> bool {
        self.candidate(index).is_ok_and(|r| {
            actual.file_size == self.file_size
                && actual.offset == r.start
                && actual.written as u64 == r.end - r.start
        })
    }
}

/// Reject an uncorroborated hint without mistaking it for verified media.
pub fn corroborate_hint(
    source: &OriginalSource,
    first: &DecodedPart,
    last: &DecodedPart,
) -> Option<UniformRanges> {
    let RangeHint {
        part_size,
        final_begin,
        final_end,
    } = source.range_hint()?;
    if part_size == 0 || part_size > MAX_ARTICLE_BYTES as u64 || part_size != first.written as u64 {
        return None;
    }
    let size = source.file_size();
    let count = (size - 1).checked_div(part_size)?.checked_add(1)?;
    if count != source.article_count() as u64 {
        return None;
    }
    let final_start = (count - 1).checked_mul(part_size)?;
    if final_begin != final_start || final_end != size {
        return None;
    }
    if first.multipart != (count > 1) || last.multipart != (count > 1) {
        return None;
    }
    let ranges = UniformRanges {
        part_size,
        file_size: size,
        count: source.article_count(),
    };
    (ranges.corroborates(0, first) && ranges.corroborates(source.article_count() - 1, last))
        .then_some(ranges)
}

/// Stream a validated half-open range through a contiguous prefix walk.
/// No hint can bypass the baseline. Actual final-part corroboration happens
/// before output, and every covering/intermediate part is checked before
/// slicing. At most N BODY opens occur; endpoint data is not fetched twice.
/// An error after output requires the authority to abort the response, never
/// claim a complete file. The sink must stream, not retain a file-sized Vec.
pub fn read_range<T: ArticleSource, C: ReadControl>(
    source: &OriginalSource,
    range: Range<u64>,
    transport: &mut T,
    control: &mut C,
    mut sink: impl FnMut(&[u8]) -> Result<(), SourceError>,
) -> Result<(), SourceError> {
    let mut walk = PrefixWalk::new(source, range)?;
    while !walk.done() {
        let (index, budget) = walk.request()?;
        let part = if index == source.article_count() - 1 && walk.last.is_some() {
            walk.last.take().ok_or(SourceError::Integrity)?
        } else {
            fetch(source, index, budget, transport, control)?
        };
        if let Some(bytes) = walk.accept(part)? {
            control.check()?;
            sink(&bytes)?;
        }
    }
    Ok(())
}

/// Async supplier for one complete, transfer-encoded article. The supplier
/// must cap collection at `MAX_ARTICLE_BYTES` before growing its buffer and
/// enforce its own operation timer. `index` is the trusted source ordinal,
/// not a client-selected article. Dropping the future cancels local work;
/// account capacity/physical close remain the supplier's responsibility.
pub trait AsyncArticleSource {
    fn open(
        &mut self,
        index: usize,
        reference: &str,
    ) -> impl Future<Output = Result<Vec<u8>, SourceError>>;
}

/// Request authority with a wakeable cancellation/deadline signal. A pending
/// implementation must register the waker; `check` alone cannot interrupt an
/// awaited article. Return the same redacted error that `check` would return.
pub trait AsyncReadControl: ReadControl {
    fn poll_cancel(&mut self, cx: &mut Context<'_>) -> Poll<SourceError>;
}

/// Lazy, bounded async prefix walk over the same validations as `read_range`.
/// No I/O happens until `next` is polled. Output is verified media, not delivery
/// evidence: only the response writer may count bytes it actually accepts.
/// An error, or dropping a pending `next`, permanently stops this reader; it
/// never retries an uncertain account operation. Drop it to discard buffers.
pub struct AsyncRangeReader<'a> {
    walk: PrefixWalk<'a>,
    stopped: bool,
}

impl<'a> AsyncRangeReader<'a> {
    pub fn new(source: &'a OriginalSource, range: Range<u64>) -> Result<Self, SourceError> {
        Ok(Self {
            walk: PrefixWalk::new(source, range)?,
            stopped: false,
        })
    }

    pub async fn next<T: AsyncArticleSource, C: AsyncReadControl>(
        &mut self,
        transport: &mut T,
        control: &mut C,
    ) -> Result<Option<Vec<u8>>, SourceError> {
        if self.stopped {
            return Err(SourceError::Cancelled);
        }
        // Fail closed even if the caller drops this future while it is pending.
        self.stopped = true;
        control.check()?;
        while !self.walk.done() {
            let (index, budget) = self.walk.request()?;
            let part = if index == self.walk.source.article_count() - 1 && self.walk.last.is_some()
            {
                self.walk.last.take().ok_or(SourceError::Integrity)?
            } else {
                control.check()?;
                let article = self.walk.source.article(index)?;
                let mut opening = pin!(async { transport.open(index, article.reference()).await });
                let encoded = poll_fn(|cx| {
                    // Cancellation wins a simultaneous-ready race, and is
                    // checked again after the supplier resolves.
                    if let Poll::Ready(error) = control.poll_cancel(cx) {
                        return Poll::Ready(Err(error));
                    }
                    opening.as_mut().poll(cx)
                })
                .await?;
                control.check()?;
                decode(self.walk.source, index, budget, &encoded)?
            };
            if let Some(bytes) = self.walk.accept(part)? {
                control.check()?;
                self.stopped = false;
                return Ok(Some(bytes));
            }
        }
        self.stopped = false;
        Ok(None)
    }
}

// Transport-independent endpoint proof, aggregate budget and contiguous cursor.
// Both drivers fetch the final article once and retain it for the prefix walk.
struct PrefixWalk<'a> {
    source: &'a OriginalSource,
    range: Range<u64>,
    last: Option<Part>,
    proved_final: bool,
    index: usize,
    position: u64,
    total_decoded: u64,
    hint: Option<UniformRanges>,
}

impl<'a> PrefixWalk<'a> {
    fn new(source: &'a OriginalSource, range: Range<u64>) -> Result<Self, SourceError> {
        if range.start >= range.end || range.end > source.file_size() {
            return Err(SourceError::Range);
        }
        Ok(Self {
            source,
            range,
            last: None,
            proved_final: false,
            index: 0,
            position: 0,
            total_decoded: 0,
            hint: None,
        })
    }

    fn done(&self) -> bool {
        self.position >= self.range.end
    }

    fn request(&self) -> Result<(usize, u64), SourceError> {
        if !self.proved_final {
            return Ok((self.source.article_count() - 1, self.source.file_size()));
        }
        if self.index >= self.source.article_count() {
            return Err(SourceError::Integrity);
        }
        if self.index == self.source.article_count() - 1 {
            return Ok((self.index, 0)); // Already decoded; never open it again.
        }
        let remaining = self
            .source
            .file_size()
            .checked_sub(self.total_decoded)
            .filter(|&n| n > 0)
            .ok_or(SourceError::Integrity)?;
        Ok((self.index, remaining))
    }

    fn accept(&mut self, part: Part) -> Result<Option<Vec<u8>>, SourceError> {
        if !self.proved_final {
            self.total_decoded = part.geometry.written as u64;
            self.last = Some(part);
            self.proved_final = true;
            return Ok(None);
        }
        if self.index != self.source.article_count() - 1 {
            self.total_decoded = self
                .total_decoded
                .checked_add(part.geometry.written as u64)
                .filter(|&n| n <= self.source.file_size())
                .ok_or(SourceError::Integrity)?;
        }
        let actual = part.geometry;
        if actual.offset != self.position {
            return Err(SourceError::Integrity);
        }
        let end = self
            .position
            .checked_add(actual.written as u64)
            .filter(|&n| n <= self.source.file_size())
            .ok_or(SourceError::Integrity)?;
        if self.index == 0 {
            // Single-part sources consume `last` before this transition.
            let last = self.last.as_ref().map_or(actual, |p| p.geometry);
            self.hint = corroborate_hint(self.source, &actual, &last);
        }
        if self
            .hint
            .is_some_and(|h| !h.corroborates(self.index, &actual))
        {
            self.hint = None;
        }
        let start_out = self.range.start.max(self.position);
        let end_out = self.range.end.min(end);
        let bytes = if start_out < end_out {
            let a = usize::try_from(start_out - self.position).map_err(|_| SourceError::Bounds)?;
            let b = usize::try_from(end_out - self.position).map_err(|_| SourceError::Bounds)?;
            // Reuse the article allocation rather than retaining a second copy.
            let mut bytes = part.bytes;
            bytes.truncate(b);
            bytes.drain(..a);
            Some(bytes)
        } else {
            None
        };
        self.position = end;
        self.index += 1;
        Ok(bytes)
    }
}
