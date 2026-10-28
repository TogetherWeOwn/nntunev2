//! Inert provider-v3 response codec and static validator (L0 foundation).
//!
//! Pure portable parsing of the exact closed wire shapes `Provider3Success`
//! and `ProviderOnly` from the pinned provider/manifest seam (S §§2, 6.1,
//! 6.2, 8). There is no runtime wiring: no Worker export, route, resolver
//! caller, signer, crypto, ledger, authority producer, environment binding,
//! or enablement. No production source may import or call this module; the
//! only permitted reference is this file's out-of-line `pub mod` export.
//!
//! # Parsing is not authorization
//!
//! A syntactically valid response is **not** an authorized response. This
//! module compares caller-supplied expected bindings exactly, but it cannot
//! authenticate the issuer, current state, SQL ownership, DO identity, lease,
//! audit acknowledgement, DNS vetting, dial peer, TLS transport, stored
//! plaintext acknowledgement, producer-side capacity eligibility, or the
//! unpublished verification BODY. All of those live prerequisites remain
//! unimplemented, and success here must never be treated as authority to
//! connect, decrypt, or deliver credentials.
//!
//! # Wire discipline
//!
//! * Whole-object cap of 8 KiB applies before parsing; oversized input is
//!   denied wholly, never truncated.
//! * Strict UTF-8 JSON: no BOM, no trailing values, no unknown or duplicate
//!   keys (duplicate detection compares unescaped key spellings), no lone
//!   surrogates, no `null` anywhere, no implicit defaults.
//! * Integer fields accept only canonical decimal lexemes (`0` or
//!   `[1-9][0-9]*`); float, exponent, leading-plus, leading-zero, and
//!   negative forms are denied without repair.
//! * Every denial renders a fixed sanitized code: `authorization_denied`
//!   when a well-formed response fails a supplied binding comparison, and
//!   `provider_unavailable` when the response itself is unusable (oversize,
//!   malformed JSON, open shapes, malformed fields). No raw parser detail,
//!   credential, or debug rendering ever escapes.
//!
//! The decoded views deliberately do not implement [`core::fmt::Debug`]:
//! provider material must stay out of capture/debug/tail/trace surfaces.

use core::fmt;

/// Whole-object refusal bound for a provider response (S §8).
pub const MAX_PROVIDER_RESPONSE_BYTES: usize = 8 * 1024;

/// Largest representable safe integer for epochs and times (2^53 − 1).
const SAFE_MAX: u64 = 9_007_199_254_740_991;

/// Supplied static bindings a response is compared against, exactly.
///
/// These are caller-provided comparison inputs, not authenticated authority.
/// Binary equality is used throughout: no trimming, case folding, Unicode
/// normalization, or UUID coercion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpectedProviderResponse {
    pub request_id: String,
    pub lease_id: String,
    pub lease_until: u64,
    pub pool_owner: String,
    pub provider_id: String,
    pub provider_version: u64,
    pub owner_epoch: u64,
    pub permission_epoch: u64,
    pub scope_id: String,
    pub operation: String,
    pub scope_digest: String,
    pub context_digest: String,
    pub plan_digest: String,
    pub cnf: KeyBinding,
    pub issuer_key: KeyBinding,
}

/// Opaque key label binding (`Cnf = Closed{kid:KeyId,kv:Epoch}`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyBinding {
    pub kid: String,
    pub kv: u64,
}

/// Inert decoded view of a validated v3 response, not an authority handle.
///
/// Neither this type nor [`ProviderOnly`] implements `Debug`, so decoded
/// provider material cannot leak through debug rendering.
pub struct Provider3Success {
    bindings: ExpectedProviderResponse,
    provider: ProviderOnly,
}

impl Provider3Success {
    /// Wire bindings decoded from the response (equal to the supplied
    /// expected tuple by construction).
    pub fn bindings(&self) -> &ExpectedProviderResponse {
        &self.bindings
    }

    /// Decoded provider-only material.
    pub fn provider(&self) -> &ProviderOnly {
        &self.provider
    }
}

/// Inert decoded view of provider-only connection material.
pub struct ProviderOnly {
    host: String,
    port: u16,
    tls: bool,
    allow_insecure_plaintext: bool,
    username: String,
    password: String,
    effective_max_connections: u8,
}

impl ProviderOnly {
    pub fn host(&self) -> &str {
        &self.host
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn tls(&self) -> bool {
        self.tls
    }

    pub fn allow_insecure_plaintext(&self) -> bool {
        self.allow_insecure_plaintext
    }

    pub fn username(&self) -> &str {
        &self.username
    }

    pub fn password(&self) -> &str {
        &self.password
    }

    pub fn effective_max_connections(&self) -> u8 {
        self.effective_max_connections
    }
}

/// Fixed sanitized denial codes (S §8); no free text, no raw diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderResponseError {
    /// Well-formed response that fails a supplied binding comparison.
    AuthorizationDenied,
    /// Unusable response: oversize, malformed JSON, open shapes, or
    /// malformed fields.
    ProviderUnavailable,
}

impl fmt::Display for ProviderResponseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ProviderResponseError::AuthorizationDenied => "authorization_denied",
            ProviderResponseError::ProviderUnavailable => "provider_unavailable",
        })
    }
}

impl std::error::Error for ProviderResponseError {}

/// Validate raw response bytes against supplied expected bindings.
///
/// Accepts only exact closed v3 shapes; see the module documentation for
/// the wire discipline. Any failure denies with a fixed code and no detail.
pub fn validate_provider_response(
    raw: &[u8],
    expected: &ExpectedProviderResponse,
) -> Result<Provider3Success, ProviderResponseError> {
    use ProviderResponseError::{AuthorizationDenied, ProviderUnavailable};
    if raw.len() > MAX_PROVIDER_RESPONSE_BYTES {
        return Err(ProviderUnavailable);
    }
    let text = core::str::from_utf8(raw).map_err(|_| ProviderUnavailable)?;
    if text.starts_with('\u{FEFF}') {
        return Err(ProviderUnavailable);
    }
    let mut parser = Parser {
        text,
        bytes: text.as_bytes(),
        pos: 0,
    };
    parser.skip_ws();
    let root = parser.parse_root().map_err(|_| ProviderUnavailable)?;
    parser.skip_ws();
    if parser.pos != parser.bytes.len() {
        return Err(ProviderUnavailable);
    }

    // Shape validation first: a malformed wire value is denied even when the
    // supplied expected tuple agrees with it.
    if root.v != 3 {
        return Err(ProviderUnavailable);
    }
    for id in [
        &root.request_id,
        &root.lease_id,
        &root.provider_id,
        &root.scope_id,
    ] {
        if !is_uuid_v4(id) {
            return Err(ProviderUnavailable);
        }
    }
    if root.lease_until > SAFE_MAX {
        return Err(ProviderUnavailable);
    }
    if !is_native_owner(&root.pool_owner) {
        return Err(ProviderUnavailable);
    }
    for epoch in [
        root.provider_version,
        root.owner_epoch,
        root.permission_epoch,
    ] {
        if !(1..=SAFE_MAX).contains(&epoch) {
            return Err(ProviderUnavailable);
        }
    }
    if root.operation != "nntp.body" && root.operation != "nntp.stat" {
        return Err(ProviderUnavailable);
    }
    for digest in [&root.scope_digest, &root.context_digest, &root.plan_digest] {
        if !is_digest(digest) {
            return Err(ProviderUnavailable);
        }
    }
    for binding in [&root.cnf, &root.issuer_key] {
        if !is_key_id(&binding.kid) || !(1..=SAFE_MAX).contains(&binding.kv) {
            return Err(ProviderUnavailable);
        }
    }
    let provider = &root.provider;
    if !is_host_shape(&provider.host) {
        return Err(ProviderUnavailable);
    }
    if provider.port != 119 && provider.port != 563 {
        return Err(ProviderUnavailable);
    }
    if !provider.tls && !provider.allow_insecure_plaintext {
        return Err(ProviderUnavailable);
    }
    if !is_credential(&provider.username, 255) || !is_credential(&provider.password, 1024) {
        return Err(ProviderUnavailable);
    }
    if !(1..=50).contains(&provider.effective_max_connections) {
        return Err(ProviderUnavailable);
    }

    // Exact binding comparison: every supplied static binding must match.
    if root.request_id != expected.request_id
        || root.lease_id != expected.lease_id
        || root.lease_until != expected.lease_until
        || root.pool_owner != expected.pool_owner
        || root.provider_id != expected.provider_id
        || root.provider_version != expected.provider_version
        || root.owner_epoch != expected.owner_epoch
        || root.permission_epoch != expected.permission_epoch
        || root.scope_id != expected.scope_id
        || root.operation != expected.operation
        || root.scope_digest != expected.scope_digest
        || root.context_digest != expected.context_digest
        || root.plan_digest != expected.plan_digest
        || root.cnf.kid != expected.cnf.kid
        || root.cnf.kv != expected.cnf.kv
        || root.issuer_key.kid != expected.issuer_key.kid
        || root.issuer_key.kv != expected.issuer_key.kv
    {
        return Err(AuthorizationDenied);
    }

    Ok(Provider3Success {
        bindings: ExpectedProviderResponse {
            request_id: root.request_id,
            lease_id: root.lease_id,
            lease_until: root.lease_until,
            pool_owner: root.pool_owner,
            provider_id: root.provider_id,
            provider_version: root.provider_version,
            owner_epoch: root.owner_epoch,
            permission_epoch: root.permission_epoch,
            scope_id: root.scope_id,
            operation: root.operation,
            scope_digest: root.scope_digest,
            context_digest: root.context_digest,
            plan_digest: root.plan_digest,
            cnf: KeyBinding {
                kid: root.cnf.kid,
                kv: root.cnf.kv,
            },
            issuer_key: KeyBinding {
                kid: root.issuer_key.kid,
                kv: root.issuer_key.kv,
            },
        },
        provider: ProviderOnly {
            host: root.provider.host,
            port: u16::try_from(root.provider.port).map_err(|_| ProviderUnavailable)?,
            tls: root.provider.tls,
            allow_insecure_plaintext: root.provider.allow_insecure_plaintext,
            username: root.provider.username,
            password: root.provider.password,
            effective_max_connections: u8::try_from(root.provider.effective_max_connections)
                .map_err(|_| ProviderUnavailable)?,
        },
    })
}

/// Lowercase UUIDv4 shape: 8-4-4-4-12 lowercase hex, version nibble `4`,
/// variant bits `10xx`.
fn is_uuid_v4(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    for (index, byte) in bytes.iter().enumerate() {
        let hex = byte.is_ascii_digit() || (b'a'..=b'f').contains(byte);
        match index {
            8 | 13 | 18 | 23 => {
                if *byte != b'-' {
                    return false;
                }
            }
            14 => {
                if *byte != b'4' {
                    return false;
                }
            }
            19 => {
                if !matches!(byte, b'8' | b'9' | b'a' | b'b') {
                    return false;
                }
            }
            _ => {
                if !hex {
                    return false;
                }
            }
        }
    }
    true
}

/// Native owner TEXT (S §2): nonempty, at most 256 UTF-8 bytes, no
/// U+0000–U+001F or U+007F; binary equality, never normalized.
fn is_native_owner(value: &str) -> bool {
    if value.is_empty() || value.len() > 256 {
        return false;
    }
    !value.chars().any(|c| c < '\u{20}' || c == '\u{7f}')
}

/// `H`: 64 lowercase hex SHA-256 characters.
fn is_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// `KeyId`: `[a-z0-9_-]`, 1…64 characters.
fn is_key_id(value: &str) -> bool {
    let len = value.len();
    (1..=64).contains(&len)
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

/// Credential shape (P1 §7): 1…`max` UTF-8 bytes, no NUL/CR/LF; the
/// NNTP line framing cannot carry those bytes, so they deny rather than
/// normalize or truncate.
fn is_credential(value: &str, max: usize) -> bool {
    if value.is_empty() || value.len() > max {
        return false;
    }
    !value.bytes().any(|b| b == 0 || b == b'\r' || b == b'\n')
}

/// Static host shape (P1 §7 lengths): 1…253 ASCII bytes of dot-separated
/// labels, each 1…63 `[A-Za-z0-9-]` without leading or trailing hyphen.
/// This is shape only, not fresh DNS resolution or peer vetting.
fn is_host_shape(value: &str) -> bool {
    let len = value.len();
    if len == 0 || len > 253 || !value.is_ascii() {
        return false;
    }
    value.split('.').all(|label| {
        let len = label.len();
        (1..=63).contains(&len)
            && label
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            && !label.starts_with('-')
            && !label.ends_with('-')
    })
}

/// Schema-directed strict JSON reader. Any deviation denies; the parser
/// never repairs input.
struct Parser<'a> {
    text: &'a str,
    bytes: &'a [u8],
    pos: usize,
}

impl Parser<'_> {
    fn skip_ws(&mut self) {
        while self.pos < self.bytes.len()
            && matches!(self.bytes[self.pos], b' ' | b'\t' | b'\n' | b'\r')
        {
            self.pos += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn expect_byte(&mut self, want: u8) -> Result<(), ()> {
        if self.peek() == Some(want) {
            self.pos += 1;
            Ok(())
        } else {
            Err(())
        }
    }

    /// Parse a JSON string with standard escapes; lone surrogates, invalid
    /// escapes, and raw control bytes deny.
    fn parse_string(&mut self) -> Result<String, ()> {
        self.expect_byte(b'"')?;
        let mut out = String::new();
        loop {
            let byte = *self.bytes.get(self.pos).ok_or(())?;
            match byte {
                b'"' => {
                    self.pos += 1;
                    return Ok(out);
                }
                b'\\' => {
                    self.pos += 1;
                    let escape = *self.bytes.get(self.pos).ok_or(())?;
                    self.pos += 1;
                    match escape {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => out.push(self.parse_unicode_escape()?),
                        _ => return Err(()),
                    }
                }
                0x00..=0x1f => return Err(()),
                0x20..=0x7f => {
                    out.push(byte as char);
                    self.pos += 1;
                }
                _ => {
                    let ch = self.text[self.pos..].chars().next().ok_or(())?;
                    out.push(ch);
                    self.pos += ch.len_utf8();
                }
            }
        }
    }

    fn parse_hex4(&mut self) -> Result<u32, ()> {
        if self.pos + 4 > self.bytes.len() {
            return Err(());
        }
        let mut value: u32 = 0;
        for i in 0..4 {
            let digit = char::from(self.bytes[self.pos + i])
                .to_digit(16)
                .ok_or(())?;
            value = value * 16 + digit;
        }
        self.pos += 4;
        Ok(value)
    }

    fn parse_unicode_escape(&mut self) -> Result<char, ()> {
        let high = self.parse_hex4()?;
        if (0xd800..0xdc00).contains(&high) {
            if self.bytes.get(self.pos) != Some(&b'\\')
                || self.bytes.get(self.pos + 1) != Some(&b'u')
            {
                return Err(());
            }
            self.pos += 2;
            let low = self.parse_hex4()?;
            if !(0xdc00..0xe000).contains(&low) {
                return Err(());
            }
            let scalar = 0x1_0000 + ((high - 0xd800) << 10) + (low - 0xdc00);
            char::from_u32(scalar).ok_or(())
        } else if (0xdc00..0xe000).contains(&high) {
            Err(())
        } else {
            char::from_u32(high).ok_or(())
        }
    }

    /// Parse a canonical unsigned decimal lexeme (`0` or `[1-9][0-9]*`).
    /// Any other number form denies without repair.
    fn parse_uint(&mut self) -> Result<u64, ()> {
        let start = self.pos;
        let first = *self.bytes.get(self.pos).ok_or(())?;
        if first == b'0' {
            self.pos += 1;
        } else if first.is_ascii_digit() {
            while self.pos < self.bytes.len() && self.bytes[self.pos].is_ascii_digit() {
                self.pos += 1;
            }
        } else {
            return Err(());
        }
        match self.bytes.get(self.pos) {
            Some(b' ' | b'\t' | b'\n' | b'\r' | b',' | b'}' | b']' | b':') | None => {}
            Some(_) => return Err(()),
        }
        self.text[start..self.pos].parse::<u64>().map_err(|_| ())
    }

    fn parse_bool(&mut self) -> Result<bool, ()> {
        if self.text[self.pos..].starts_with("true") {
            self.pos += 4;
            Ok(true)
        } else if self.text[self.pos..].starts_with("false") {
            self.pos += 5;
            Ok(false)
        } else {
            Err(())
        }
    }

    fn parse_members(
        &mut self,
        mut on_field: impl FnMut(&mut Self, &str) -> Result<(), ()>,
    ) -> Result<(), ()> {
        self.expect_byte(b'{')?;
        self.skip_ws();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Ok(());
        }
        loop {
            self.skip_ws();
            let key = self.parse_string()?;
            self.skip_ws();
            self.expect_byte(b':')?;
            self.skip_ws();
            on_field(self, &key)?;
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b'}') => {
                    self.pos += 1;
                    return Ok(());
                }
                _ => return Err(()),
            }
        }
    }

    fn parse_key_binding(&mut self) -> Result<RawKeyBinding, ()> {
        let mut binding = RawKeyBindingPartial {
            kid: None,
            kv: None,
        };
        self.parse_members(|parser, key| {
            match key {
                "kid" => {
                    if binding.kid.is_some() {
                        return Err(());
                    }
                    binding.kid = Some(parser.parse_string()?);
                }
                "kv" => {
                    if binding.kv.is_some() {
                        return Err(());
                    }
                    binding.kv = Some(parser.parse_uint()?);
                }
                _ => return Err(()),
            }
            Ok(())
        })?;
        Ok(RawKeyBinding {
            kid: binding.kid.ok_or(())?,
            kv: binding.kv.ok_or(())?,
        })
    }

    fn parse_provider(&mut self) -> Result<RawProvider, ()> {
        let mut provider = RawProviderPartial::default();
        self.parse_members(|parser, key| {
            match key {
                "host" => {
                    if provider.host.is_some() {
                        return Err(());
                    }
                    provider.host = Some(parser.parse_string()?);
                }
                "port" => {
                    if provider.port.is_some() {
                        return Err(());
                    }
                    provider.port = Some(parser.parse_uint()?);
                }
                "tls" => {
                    if provider.tls.is_some() {
                        return Err(());
                    }
                    provider.tls = Some(parser.parse_bool()?);
                }
                "allow_insecure_plaintext" => {
                    if provider.allow_insecure_plaintext.is_some() {
                        return Err(());
                    }
                    provider.allow_insecure_plaintext = Some(parser.parse_bool()?);
                }
                "username" => {
                    if provider.username.is_some() {
                        return Err(());
                    }
                    provider.username = Some(parser.parse_string()?);
                }
                "password" => {
                    if provider.password.is_some() {
                        return Err(());
                    }
                    provider.password = Some(parser.parse_string()?);
                }
                "effective_max_connections" => {
                    if provider.effective_max_connections.is_some() {
                        return Err(());
                    }
                    provider.effective_max_connections = Some(parser.parse_uint()?);
                }
                _ => return Err(()),
            }
            Ok(())
        })?;
        Ok(RawProvider {
            host: provider.host.ok_or(())?,
            port: provider.port.ok_or(())?,
            tls: provider.tls.ok_or(())?,
            allow_insecure_plaintext: provider.allow_insecure_plaintext.ok_or(())?,
            username: provider.username.ok_or(())?,
            password: provider.password.ok_or(())?,
            effective_max_connections: provider.effective_max_connections.ok_or(())?,
        })
    }

    fn parse_root(&mut self) -> Result<RawRoot, ()> {
        let mut root = RawRootPartial::default();
        self.parse_members(|parser, key| {
            match key {
                "v" => {
                    if root.v.is_some() {
                        return Err(());
                    }
                    root.v = Some(parser.parse_uint()?);
                }
                "request_id" => {
                    if root.request_id.is_some() {
                        return Err(());
                    }
                    root.request_id = Some(parser.parse_string()?);
                }
                "lease_id" => {
                    if root.lease_id.is_some() {
                        return Err(());
                    }
                    root.lease_id = Some(parser.parse_string()?);
                }
                "lease_until" => {
                    if root.lease_until.is_some() {
                        return Err(());
                    }
                    root.lease_until = Some(parser.parse_uint()?);
                }
                "pool_owner" => {
                    if root.pool_owner.is_some() {
                        return Err(());
                    }
                    root.pool_owner = Some(parser.parse_string()?);
                }
                "provider_id" => {
                    if root.provider_id.is_some() {
                        return Err(());
                    }
                    root.provider_id = Some(parser.parse_string()?);
                }
                "provider_version" => {
                    if root.provider_version.is_some() {
                        return Err(());
                    }
                    root.provider_version = Some(parser.parse_uint()?);
                }
                "owner_epoch" => {
                    if root.owner_epoch.is_some() {
                        return Err(());
                    }
                    root.owner_epoch = Some(parser.parse_uint()?);
                }
                "permission_epoch" => {
                    if root.permission_epoch.is_some() {
                        return Err(());
                    }
                    root.permission_epoch = Some(parser.parse_uint()?);
                }
                "scope_id" => {
                    if root.scope_id.is_some() {
                        return Err(());
                    }
                    root.scope_id = Some(parser.parse_string()?);
                }
                "operation" => {
                    if root.operation.is_some() {
                        return Err(());
                    }
                    root.operation = Some(parser.parse_string()?);
                }
                "provider" => {
                    if root.provider.is_some() {
                        return Err(());
                    }
                    root.provider = Some(parser.parse_provider()?);
                }
                "scope_digest" => {
                    if root.scope_digest.is_some() {
                        return Err(());
                    }
                    root.scope_digest = Some(parser.parse_string()?);
                }
                "context_digest" => {
                    if root.context_digest.is_some() {
                        return Err(());
                    }
                    root.context_digest = Some(parser.parse_string()?);
                }
                "plan_digest" => {
                    if root.plan_digest.is_some() {
                        return Err(());
                    }
                    root.plan_digest = Some(parser.parse_string()?);
                }
                "cnf" => {
                    if root.cnf.is_some() {
                        return Err(());
                    }
                    root.cnf = Some(parser.parse_key_binding()?);
                }
                "issuer_key" => {
                    if root.issuer_key.is_some() {
                        return Err(());
                    }
                    root.issuer_key = Some(parser.parse_key_binding()?);
                }
                _ => return Err(()),
            }
            Ok(())
        })?;
        Ok(RawRoot {
            v: root.v.ok_or(())?,
            request_id: root.request_id.ok_or(())?,
            lease_id: root.lease_id.ok_or(())?,
            lease_until: root.lease_until.ok_or(())?,
            pool_owner: root.pool_owner.ok_or(())?,
            provider_id: root.provider_id.ok_or(())?,
            provider_version: root.provider_version.ok_or(())?,
            owner_epoch: root.owner_epoch.ok_or(())?,
            permission_epoch: root.permission_epoch.ok_or(())?,
            scope_id: root.scope_id.ok_or(())?,
            operation: root.operation.ok_or(())?,
            provider: root.provider.ok_or(())?,
            scope_digest: root.scope_digest.ok_or(())?,
            context_digest: root.context_digest.ok_or(())?,
            plan_digest: root.plan_digest.ok_or(())?,
            cnf: root.cnf.ok_or(())?,
            issuer_key: root.issuer_key.ok_or(())?,
        })
    }
}

struct RawKeyBinding {
    kid: String,
    kv: u64,
}

struct RawKeyBindingPartial {
    kid: Option<String>,
    kv: Option<u64>,
}

struct RawProvider {
    host: String,
    port: u64,
    tls: bool,
    allow_insecure_plaintext: bool,
    username: String,
    password: String,
    effective_max_connections: u64,
}

#[derive(Default)]
struct RawProviderPartial {
    host: Option<String>,
    port: Option<u64>,
    tls: Option<bool>,
    allow_insecure_plaintext: Option<bool>,
    username: Option<String>,
    password: Option<String>,
    effective_max_connections: Option<u64>,
}

struct RawRoot {
    v: u64,
    request_id: String,
    lease_id: String,
    lease_until: u64,
    pool_owner: String,
    provider_id: String,
    provider_version: u64,
    owner_epoch: u64,
    permission_epoch: u64,
    scope_id: String,
    operation: String,
    provider: RawProvider,
    scope_digest: String,
    context_digest: String,
    plan_digest: String,
    cnf: RawKeyBinding,
    issuer_key: RawKeyBinding,
}

#[derive(Default)]
struct RawRootPartial {
    v: Option<u64>,
    request_id: Option<String>,
    lease_id: Option<String>,
    lease_until: Option<u64>,
    pool_owner: Option<String>,
    provider_id: Option<String>,
    provider_version: Option<u64>,
    owner_epoch: Option<u64>,
    permission_epoch: Option<u64>,
    scope_id: Option<String>,
    operation: Option<String>,
    provider: Option<RawProvider>,
    scope_digest: Option<String>,
    context_digest: Option<String>,
    plan_digest: Option<String>,
    cnf: Option<RawKeyBinding>,
    issuer_key: Option<RawKeyBinding>,
}
