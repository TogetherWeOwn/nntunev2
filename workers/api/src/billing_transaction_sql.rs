//! Unused SQL primitives for a future coupled billing transaction.
//!
//! These statements alone do not implement a transaction or protect history.
//! The lock and lookup must run on the same explicitly READ COMMITTED backend,
//! and the lookup must be a separate statement after the lock is acquired.
//! Production writes must resolve to this same trusted relation. Disposable
//! fixtures need their own trusted relation-specific SQL; no schema name is an
//! input to these production primitives.

/// Excludes subscription DML until the enclosing transaction ends.
pub const LOCK_SUBSCRIPTIONS: &str = "LOCK TABLE public.subscriptions IN SHARE ROW EXCLUSIVE MODE";

/// Global key lookup and the existing upsert's conflict-WHERE suppression
/// predicate. `IS TRUE` makes SQL NULL semantics yield an unambiguous boolean.
/// The owner is deliberately not part of the lookup predicate.
pub const LOOKUP_TARGET: &str = "SELECT user_id, stripe_subscription_id, \
    (to_timestamp($2::bigint) IS NOT NULL AND paid_through_at IS NOT NULL AND \
    (to_timestamp($2::bigint) < paid_through_at OR \
    (to_timestamp($2::bigint) = paid_through_at AND $3::text IS NULL AND \
    paid_invoice_id IS NOT NULL))) IS TRUE AS predicate_stale \
    FROM public.subscriptions WHERE stripe_subscription_id = $1";

/// Ordered parameters for the global lookup: key, incoming paid-through epoch,
/// incoming paid invoice ID. Do not use an owner filter to hide foreign rows.
#[derive(Debug, PartialEq, Eq)]
pub struct LookupBinds<'a> {
    pub subscription_id: &'a str,
    pub paid_through_seconds: Option<i64>,
    pub paid_invoice_id: Option<&'a str>,
}

impl<'a> LookupBinds<'a> {
    pub fn ordered(&self) -> (&str, Option<i64>, Option<&str>) {
        (
            self.subscription_id,
            self.paid_through_seconds,
            self.paid_invoice_id,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_relation_and_global_lookup_are_pinned() {
        assert_eq!(
            LOCK_SUBSCRIPTIONS,
            "LOCK TABLE public.subscriptions IN SHARE ROW EXCLUSIVE MODE"
        );
        assert_eq!(LOOKUP_TARGET, "SELECT user_id, stripe_subscription_id, (to_timestamp($2::bigint) IS NOT NULL AND paid_through_at IS NOT NULL AND (to_timestamp($2::bigint) < paid_through_at OR (to_timestamp($2::bigint) = paid_through_at AND $3::text IS NULL AND paid_invoice_id IS NOT NULL))) IS TRUE AS predicate_stale FROM public.subscriptions WHERE stripe_subscription_id = $1");
    }

    #[test]
    fn lookup_bind_order_preserves_nullable_inputs() {
        let binds = LookupBinds {
            subscription_id: "sub-example",
            paid_through_seconds: Some(42),
            paid_invoice_id: Some("invoice-example"),
        };
        assert_eq!(
            binds.ordered(),
            ("sub-example", Some(42), Some("invoice-example"))
        );
        let nulls = LookupBinds {
            subscription_id: "sub-null",
            paid_through_seconds: None,
            paid_invoice_id: None,
        };
        assert_eq!(nulls.ordered(), ("sub-null", None, None));
    }
}
