//! Lease-local allowlist of compiled cases. Slots and overlap belong to the runner.

use worker::js_sys::JsString;

use super::is_ascii_literal;
use crate::subsonic_staging::SyntheticCatalogue;

const CASE_IDS: [&str; 21] = [
    "empty",
    "small",
    "rows-below",
    "rows-exact",
    "rows-over",
    "charge-below",
    "charge-exact",
    "charge-over",
    "text-below",
    "text-exact",
    "text-over",
    "output-json-below",
    "output-json-exact",
    "output-json-over",
    "output-xml-below",
    "output-xml-exact",
    "output-xml-over",
    "output-jsonp-below",
    "output-jsonp-exact",
    "output-jsonp-over",
    "large-mixed",
];

// wasm32 GET getArtists (JSON), small fixture: raw rows + owned copy +
// snapshot + Value graph + complete response buffer. See the fixture crosswalk.
const ARTIST_INDEX_CHARGE: usize = 4_024;

pub(super) struct Schedule(u32);

impl Schedule {
    pub(super) fn parse(list: &str) -> Option<Self> {
        let mut included = 0;
        for id in list.split(',') {
            let bit = 1 << CASE_IDS.iter().position(|known| *known == id)?;
            if included & bit != 0 {
                return None;
            }
            included |= bit;
        }
        Some(Self(included))
    }

    pub(super) fn resolve(self, selector: &JsString) -> Option<Case> {
        CASE_IDS.iter().enumerate().find_map(|(i, &id)| {
            (self.0 & (1 << i) != 0 && is_ascii_literal(selector, id)).then_some(Case(id))
        })
    }
}

#[derive(Clone, Copy)]
pub(super) struct Case(&'static str);

impl Case {
    pub(super) fn headroom(self) -> Option<usize> {
        match self.0 {
            "charge-below" => Some(ARTIST_INDEX_CHARGE + 1),
            "charge-exact" => Some(ARTIST_INDEX_CHARGE),
            "charge-over" => Some(ARTIST_INDEX_CHARGE - 1),
            _ => None,
        }
    }

    pub(super) fn catalogue(self) -> Option<SyntheticCatalogue> {
        SyntheticCatalogue::from_trusted_id(if self.headroom().is_some() {
            "small"
        } else {
            self.0
        })
    }
}
