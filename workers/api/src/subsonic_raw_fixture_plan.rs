//! Accounting-only synthetic fixture plan. This is not an allocator proof or
//! an admission token: nonempty construction remains unavailable.

use std::alloc::Layout;

use nntune_subsonic::construction_budget::{BudgetExceeded, ConstructionBudget};

use crate::catalogue::{ArtistDbRow, ReleaseDbRow, TrackDbRow};

// These ceilings may be lowered by a test or future caller, never raised.
const MAX_ROWS: usize = 1_024;
const MAX_TEXT: usize = 1_024;
const MAX_AGGREGATE: usize = 262_144;

#[derive(Clone, Copy)]
#[allow(dead_code)]
enum OptionalText {
    None,
    Empty,
    Present,
}

#[derive(Clone, Copy)]
#[allow(dead_code)]
struct Fixture {
    artists: usize,
    releases: usize,
    tracks: usize,
    empty_text: bool,
    optional: OptionalText,
}

#[derive(Clone, Copy)]
#[allow(dead_code)]
struct Limits {
    rows_per_kind: usize,
    text_bytes: usize,
    aggregate_bytes: usize,
}

type FixtureFields = (
    [Option<&'static str>; 5],
    [Option<&'static str>; 5],
    [Option<&'static str>; 6],
);

// Every field comes from literals in this module, never from a caller-supplied
// 'static reference. Repeating a row repeats its charges, even for equal text.
impl Fixture {
    fn fields(self) -> FixtureFields {
        let text = if self.empty_text { "" } else { "fixture" };
        let optional = match self.optional {
            OptionalText::None => None,
            OptionalText::Empty => Some(""),
            OptionalText::Present => Some(text),
        };
        (
            [Some(text), Some(text), Some(text), optional, optional],
            [Some(text), Some(text), optional, optional, optional],
            [
                Some(text),
                Some(text),
                Some(text),
                Some(text),
                optional,
                optional,
            ],
        )
    }
}

fn row_layout<T>(count: usize) -> Result<(), BudgetExceeded> {
    let layout = Layout::array::<T>(count).map_err(|_| BudgetExceeded)?;
    if layout.align() > 8 {
        return Err(BudgetExceeded);
    }
    Ok(())
}

fn charge_rows<T>(budget: &mut ConstructionBudget, count: usize) -> Result<(), BudgetExceeded> {
    row_layout::<T>(count)?;
    if count != 0 && std::mem::size_of::<T>() != 0 {
        budget.charge(count, std::mem::size_of::<T>(), std::mem::align_of::<T>())?;
    }
    Ok(())
}

#[allow(dead_code)]
fn plan(
    budget: &mut ConstructionBudget,
    fixture: Fixture,
    limits: Limits,
) -> Result<(), BudgetExceeded> {
    let counts = [fixture.artists, fixture.releases, fixture.tracks];
    if limits.rows_per_kind > MAX_ROWS
        || limits.text_bytes > MAX_TEXT
        || limits.aggregate_bytes > MAX_AGGREGATE
        || counts.iter().any(|&n| n > limits.rows_per_kind)
    {
        return Err(BudgetExceeded);
    }
    let total_rows = counts
        .iter()
        .try_fold(0usize, |sum, &n| sum.checked_add(n))
        .ok_or(BudgetExceeded)?;
    if total_rows > MAX_ROWS.checked_mul(3).ok_or(BudgetExceeded)? {
        return Err(BudgetExceeded);
    }
    row_layout::<ArtistDbRow>(fixture.artists)?;
    row_layout::<ReleaseDbRow>(fixture.releases)?;
    row_layout::<TrackDbRow>(fixture.tracks)?;

    let (artists, releases, tracks) = fixture.fields();
    let groups: [(usize, &[Option<&str>]); 3] = [
        (fixture.artists, &artists),
        (fixture.releases, &releases),
        (fixture.tracks, &tracks),
    ];
    let mut aggregate = 0usize;
    for (count, fields) in groups {
        if count == 0 {
            continue;
        }
        for value in fields.iter().flatten() {
            let len = value.len();
            if len > limits.text_bytes {
                return Err(BudgetExceeded);
            }
            aggregate = aggregate
                .checked_add(len.checked_mul(count).ok_or(BudgetExceeded)?)
                .ok_or(BudgetExceeded)?;
        }
    }
    if aggregate > limits.aggregate_bytes {
        return Err(BudgetExceeded);
    }

    // All validation precedes charging. Charges are cumulative, including any
    // prior owners; discard the ledger if any charge fails. No allocation occurs.
    charge_rows::<ArtistDbRow>(budget, fixture.artists)?;
    charge_rows::<ReleaseDbRow>(budget, fixture.releases)?;
    charge_rows::<TrackDbRow>(budget, fixture.tracks)?;
    for (count, fields) in groups {
        for _ in 0..count {
            for value in fields.iter().flatten() {
                if !value.is_empty() {
                    budget.charge(value.len(), 1, 1)?;
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits() -> Limits {
        Limits {
            rows_per_kind: MAX_ROWS,
            text_bytes: MAX_TEXT,
            aggregate_bytes: MAX_AGGREGATE,
        }
    }

    fn fixture(optional: OptionalText) -> Fixture {
        Fixture {
            artists: 1,
            releases: 1,
            tracks: 1,
            empty_text: false,
            optional,
        }
    }

    #[test]
    fn charges_all_rows_fields_duplicates_and_prior_balance() {
        let f = fixture(OptionalText::Present);
        let mut expected = ConstructionBudget::new(usize::MAX);
        expected.charge(1, 1, 1).unwrap();
        for (size, align) in [
            (
                std::mem::size_of::<ArtistDbRow>(),
                std::mem::align_of::<ArtistDbRow>(),
            ),
            (
                std::mem::size_of::<ReleaseDbRow>(),
                std::mem::align_of::<ReleaseDbRow>(),
            ),
            (
                std::mem::size_of::<TrackDbRow>(),
                std::mem::align_of::<TrackDbRow>(),
            ),
        ] {
            expected.charge(1, size, align).unwrap();
        }
        for _ in 0..16 {
            expected.charge(7, 1, 1).unwrap();
        }
        let total = expected.charged();
        let mut exact = ConstructionBudget::new(total);
        exact.charge(1, 1, 1).unwrap();
        assert_eq!(plan(&mut exact, f, limits()), Ok(()));
        assert_eq!(exact.charged(), total);
        let mut below = ConstructionBudget::new(total - 1);
        below.charge(1, 1, 1).unwrap();
        assert_eq!(plan(&mut below, f, limits()), Err(BudgetExceeded));
        let mut higher = ConstructionBudget::new(total + 1);
        higher.charge(1, 1, 1).unwrap();
        assert_eq!(plan(&mut higher, f, limits()), Ok(()));
    }

    #[test]
    fn empty_and_absent_optionals_are_distinct_but_allocate_nothing() {
        let none = fixture(OptionalText::None);
        let empty = fixture(OptionalText::Empty);
        assert_eq!(none.fields().0[3], None);
        assert_eq!(empty.fields().0[3], Some(""));
        let mut a = ConstructionBudget::new(usize::MAX);
        let mut b = ConstructionBudget::new(usize::MAX);
        plan(&mut a, none, limits()).unwrap();
        plan(&mut b, empty, limits()).unwrap();
        assert_eq!(a.charged(), b.charged());
        let mut c = ConstructionBudget::new(usize::MAX);
        plan(&mut c, fixture(OptionalText::Present), limits()).unwrap();
        assert!(c.charged() > a.charged());
        let mut d = ConstructionBudget::new(usize::MAX);
        plan(
            &mut d,
            Fixture {
                empty_text: true,
                ..empty
            },
            limits(),
        )
        .unwrap();
        assert!(d.charged() > 0);
        let mut zero = ConstructionBudget::new(0);
        assert_eq!(
            plan(
                &mut zero,
                Fixture {
                    artists: 0,
                    releases: 0,
                    tracks: 0,
                    ..empty
                },
                limits()
            ),
            Ok(())
        );
    }

    #[test]
    fn limits_and_layout_fail_closed() {
        let f = fixture(OptionalText::Present);
        for l in [
            Limits {
                rows_per_kind: 0,
                ..limits()
            },
            Limits {
                text_bytes: 6,
                ..limits()
            },
            Limits {
                aggregate_bytes: 111,
                ..limits()
            },
            Limits {
                rows_per_kind: MAX_ROWS + 1,
                ..limits()
            },
        ] {
            let mut b = ConstructionBudget::new(usize::MAX);
            assert_eq!(plan(&mut b, f, l), Err(BudgetExceeded));
            assert_eq!(b.charged(), 0);
        }
        let mut b = ConstructionBudget::new(usize::MAX);
        assert_eq!(
            plan(
                &mut b,
                Fixture {
                    artists: usize::MAX,
                    ..f
                },
                limits()
            ),
            Err(BudgetExceeded)
        );
        assert_eq!(row_layout::<u64>(usize::MAX), Err(BudgetExceeded));
        #[repr(align(16))]
        struct Wide;
        assert_eq!(row_layout::<Wide>(1), Err(BudgetExceeded));
        let mut b = ConstructionBudget::new(usize::MAX);
        assert_eq!(b.charge(usize::MAX, 2, 1), Err(BudgetExceeded));
    }
}
