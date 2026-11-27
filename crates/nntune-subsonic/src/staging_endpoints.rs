//! Charged endpoint documents for the unmounted staging adapter.

use crate::construction_budget::{BudgetExceeded, ConstructionBudget};
use crate::envelope::{charged_error_document, charged_ok_document, code};
use crate::value::Value;

// Match the staging account projection's octet limit.
const MAX_LICENSE_EMAIL_BYTES: usize = 1_024;

/// Construct a document for an already-authenticated request. The caller
/// admits the account email before dispatch. These endpoints do not read the
/// catalogue; `None` does not authorize falling back to the unbounded path.
pub fn basic_document(
    budget: &mut ConstructionBudget,
    name: &str,
    email: &str,
) -> Result<Option<Value>, BudgetExceeded> {
    let document = match name {
        "ping" => charged_ok_document(budget, [])?,
        "getLicense" if !valid_license_email(email) => {
            charged_error_document(budget, code::GENERIC, "License unavailable.")?
        }
        "getLicense" => {
            let email = Value::charged_str(budget, email)?;
            let license =
                Value::charged_map(budget, [("valid", Value::Bool(true)), ("email", email)])?;
            charged_ok_document(budget, [("license", license)])?
        }
        "getMusicFolders" => {
            let mut folders = budget.allocate_exact(1)?;
            let name = Value::charged_str(budget, "Music")?;
            folders.push(Value::charged_map(
                budget,
                [("id", Value::Int(1)), ("name", name)],
            )?);
            let payload = Value::charged_map(budget, [("musicFolder", Value::List(folders))])?;
            charged_ok_document(budget, [("musicFolders", payload)])?
        }
        _ => return Ok(None),
    };
    Ok(Some(document))
}

fn valid_license_email(email: &str) -> bool {
    (1..=MAX_LICENSE_EMAIL_BYTES).contains(&email.len())
        && !email.trim().is_empty()
        && !email
            .chars()
            .any(|ch| ch.is_control() || matches!(ch, '\u{fffe}' | '\u{ffff}'))
}

#[cfg(any(test, feature = "ledger-wasm-test"))]
#[cfg_attr(test, test)]
#[doc(hidden)]
pub fn basic_documents_match_dispatch_and_charge_exact_capacity() {
    use crate::auth::Identity;
    use crate::endpoints::{handle_endpoint, EndpointResult, Params};
    use crate::envelope::{charged_error_document, code, error_document, ok_document};
    use crate::library::MemStore;

    fn charge_graph(budget: &mut ConstructionBudget, value: &Value) {
        fn charge_exact<T>(budget: &mut ConstructionBudget, len: usize, capacity: usize) {
            assert_eq!(len, capacity);
            if capacity != 0 {
                budget
                    .charge(
                        capacity,
                        std::mem::size_of::<T>(),
                        std::mem::align_of::<T>(),
                    )
                    .unwrap();
            }
        }
        match value {
            Value::Map(entries) => {
                charge_exact::<(String, Value)>(budget, entries.len(), entries.capacity());
                for (key, child) in entries {
                    charge_exact::<u8>(budget, key.len(), key.capacity());
                    charge_graph(budget, child);
                }
            }
            Value::List(entries) => {
                charge_exact::<Value>(budget, entries.len(), entries.capacity());
                for child in entries {
                    charge_graph(budget, child);
                }
            }
            Value::Str(text) => charge_exact::<u8>(budget, text.len(), text.capacity()),
            _ => {}
        }
    }

    fn check_document(
        build: impl Fn(&mut ConstructionBudget) -> Result<Value, BudgetExceeded>,
        legacy: Value,
    ) {
        let mut expected = ConstructionBudget::new(usize::MAX);
        expected.charge(8, 1, 1).unwrap();
        let mut probe = ConstructionBudget::new(usize::MAX);
        probe.charge(8, 1, 1).unwrap();
        let doc = build(&mut probe).unwrap();
        assert_eq!(doc, legacy);
        charge_graph(&mut expected, &doc);
        assert_eq!(probe.charged(), expected.charged());
        let mut short = ConstructionBudget::new(expected.charged() - 1);
        short.charge(8, 1, 1).unwrap();
        assert_eq!(build(&mut short), Err(BudgetExceeded));
        let mut exact = ConstructionBudget::new(expected.charged());
        exact.charge(8, 1, 1).unwrap();
        assert_eq!(build(&mut exact), Ok(doc));
        assert_eq!(exact.charged(), expected.charged());
    }

    // Compare the graph and envelope order against ordinary dispatch.
    let identity = Identity {
        email: "<&é@example.test".into(),
        ..Identity::for_public_route()
    };
    for name in ["ping", "getLicense", "getMusicFolders"] {
        let EndpointResult::Doc(payload) = handle_endpoint(
            &mut MemStore::empty(),
            &identity,
            name,
            &Params::default(),
            false,
        ) else {
            panic!("expected document")
        };
        check_document(
            |budget| basic_document(budget, name, &identity.email).map(Option::unwrap),
            ok_document(payload),
        );
    }
    let mut budget = ConstructionBudget::new(0);
    assert_eq!(basic_document(&mut budget, "getAlbum", ""), Ok(None));
    let mut overflow = ConstructionBudget::new(usize::MAX);
    overflow.charge(usize::MAX - 31, 1, 1).unwrap();
    let prior = overflow.charged();
    assert_eq!(
        basic_document(&mut overflow, "ping", ""),
        Err(BudgetExceeded)
    );
    assert_eq!(overflow.charged(), prior);

    let message = "Required parameter is missing.";
    let error_code = code::REQUIRED_PARAM_MISSING;
    check_document(
        |budget| charged_error_document(budget, error_code, message),
        error_document(error_code, message),
    );
}
