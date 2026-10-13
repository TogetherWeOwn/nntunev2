//! Credential handling, ported from v1 `AuthenticateSubsonicRequests` +
//! `SubsonicCredentials` (spec section 5.3 behaviour).
//!
//! Accepted, in order: per-client `apiKey`; legacy `u` with `p`; token `u`
//! with `t` and `s`. Raw values decide presence (v1 deliberately passes them
//! through uncoerced so `?apiKey[]=x`-style smuggling cannot look absent).
//! Failures are Subsonic 40, except token-without-salt which is 41. The `v`
//! (client protocol version) parameter is accepted and ignored: v1
//! authenticates before any version gate, and clients in the wild send a
//! spread of values.

use crate::envelope::code;

/// Authenticated identity handed to endpoint handlers.
#[derive(Clone, Debug, PartialEq)]
pub struct Identity {
    pub username: String,
    pub email: String,
}

/// Raw credential parameters straight off the query string.
#[derive(Clone, Debug, Default)]
pub struct Credentials {
    pub api_key: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
    pub token: Option<String>,
    pub salt: Option<String>,
}

impl Credentials {
    pub fn from_params(get: impl Fn(&str) -> Option<String>) -> Credentials {
        Credentials {
            api_key: get("apiKey"),
            username: get("u"),
            password: get("p"),
            token: get("t"),
            salt: get("s"),
        }
    }

    fn present(v: &Option<String>) -> bool {
        v.as_deref().map(|s| !s.is_empty()).unwrap_or(false)
    }
}

/// Credential verification against the backing store. The PlanetScale
/// implementation lives in the worker shell; tests use the in-memory store.
pub trait AuthStore {
    fn verify_api_key(&self, api_key: &str) -> Option<Identity>;
    fn verify_password(&self, username: &str, password: &str) -> Option<Identity>;
    fn verify_token(&self, username: &str, token: &str, salt: &str) -> Option<Identity>;
}

/// Check credentials, returning the identity or `(code, message)`.
pub fn authenticate<S: AuthStore>(
    store: &S,
    creds: &Credentials,
) -> Result<Identity, (i64, String)> {
    if Credentials::present(&creds.api_key) {
        let key = creds.api_key.clone().unwrap_or_default();
        return store
            .verify_api_key(&key)
            .ok_or((code::WRONG_CREDENTIALS, wrong_creds()));
    }
    if !Credentials::present(&creds.username) {
        return Err((code::WRONG_CREDENTIALS, wrong_creds()));
    }
    let username = creds.username.clone().unwrap_or_default();
    if Credentials::present(&creds.token) {
        if !Credentials::present(&creds.salt) {
            return Err((
                code::TOKEN_AUTH_UNSUPPORTED,
                "Token authentication requires salt.".to_string(),
            ));
        }
        let token = creds.token.clone().unwrap_or_default();
        let salt = creds.salt.clone().unwrap_or_default();
        return store
            .verify_token(&username, &token, &salt)
            .ok_or((code::WRONG_CREDENTIALS, wrong_creds()));
    }
    if !Credentials::present(&creds.password) {
        return Err((code::WRONG_CREDENTIALS, wrong_creds()));
    }
    let password = creds.password.clone().unwrap_or_default();
    store
        .verify_password(&username, &password)
        .ok_or((code::WRONG_CREDENTIALS, wrong_creds()))
}

fn wrong_creds() -> String {
    "Wrong username or password.".to_string()
}

/// MD5 hex digest (RFC 1321), for `t = md5(password + salt)` token auth.
/// A checksum compatibility helper, not a security primitive.
pub fn md5_hex(input: &[u8]) -> String {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5,
        9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10,
        15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    const K: [u32; 64] = [
        0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a, 0xa8304613,
        0xfd469501, 0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be, 0x6b901122, 0xfd987193,
        0xa679438e, 0x49b40821, 0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa, 0xd62f105d,
        0x02441453, 0xd8a1e681, 0xe7d3fbc8, 0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed,
        0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a, 0xfffa3942, 0x8771f681, 0x6d9d6122,
        0xfde5380c, 0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70, 0x289b7ec6, 0xeaa127fa,
        0xd4ef3085, 0x04881d05, 0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665, 0xf4292244,
        0x432aff97, 0xab9423a7, 0xfc93a039, 0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
        0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1, 0xf7537e82, 0xbd3af235, 0x2ad7d2bb,
        0xeb86d391,
    ];

    let mut msg = input.to_vec();
    let bit_len = (msg.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_le_bytes());

    let (mut a0, mut b0, mut c0, mut d0): (u32, u32, u32, u32) =
        (0x67452301, 0xefcdab89, 0x98badcfe, 0x10325476);

    for chunk in msg.as_chunks::<64>().0 {
        let mut m = [0u32; 16];
        for (i, word) in m.iter_mut().enumerate() {
            *word = u32::from_le_bytes([
                chunk[4 * i],
                chunk[4 * i + 1],
                chunk[4 * i + 2],
                chunk[4 * i + 3],
            ]);
        }
        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
        for i in 0..64 {
            let (f, g) = if i < 16 {
                ((b & c) | ((!b) & d), i)
            } else if i < 32 {
                ((d & b) | ((!d) & c), (5 * i + 1) % 16)
            } else if i < 48 {
                (b ^ c ^ d, (3 * i + 5) % 16)
            } else {
                (c ^ (b | (!d)), (7 * i) % 16)
            };
            let tmp = d;
            d = c;
            c = b;
            b = b.wrapping_add(
                a.wrapping_add(f)
                    .wrapping_add(K[i])
                    .wrapping_add(m[g])
                    .rotate_left(S[i]),
            );
            a = tmp;
        }
        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }

    let mut out = String::with_capacity(32);
    for word in [a0, b0, c0, d0] {
        for byte in word.to_le_bytes() {
            out.push_str(&format!("{byte:02x}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Stub;
    impl AuthStore for Stub {
        fn verify_api_key(&self, key: &str) -> Option<Identity> {
            (key == "k").then(|| Identity {
                username: "u".into(),
                email: "u@x".into(),
            })
        }
        fn verify_password(&self, u: &str, p: &str) -> Option<Identity> {
            (u == "u" && p == "p").then(|| Identity {
                username: "u".into(),
                email: "u@x".into(),
            })
        }
        fn verify_token(&self, u: &str, t: &str, s: &str) -> Option<Identity> {
            (u == "u" && t == md5_hex(format!("p{s}").as_bytes())).then(|| Identity {
                username: "u".into(),
                email: "u@x".into(),
            })
        }
    }

    fn creds(api_key: &str, u: &str, p: &str, t: &str, s: &str) -> Credentials {
        let opt = |v: &str| {
            if v.is_empty() {
                None
            } else {
                Some(v.to_string())
            }
        };
        Credentials {
            api_key: opt(api_key),
            username: opt(u),
            password: opt(p),
            token: opt(t),
            salt: opt(s),
        }
    }

    #[test]
    fn md5_matches_rfc_vector() {
        assert_eq!(md5_hex(b""), "d41d8cd98f00b204e9800998ecf8427e");
        // Verified against `md5sum` and Python `hashlib` (both agree).
        assert_eq!(md5_hex(b"abc"), "900150983cd24fb0d6963f7d28e17f72");
    }

    #[test]
    fn auth_order_and_codes() {
        let s = Stub;
        assert!(authenticate(&s, &creds("k", "", "", "", "")).is_ok());
        assert_eq!(
            authenticate(&s, &creds("bad", "", "", "", "")),
            Err((40, wrong_creds()))
        );
        assert_eq!(
            authenticate(&s, &creds("", "", "", "", "")),
            Err((40, wrong_creds()))
        );
        assert!(authenticate(&s, &creds("", "u", "p", "", "")).is_ok());
        assert_eq!(
            authenticate(&s, &creds("", "u", "t", "", "")),
            Err((40, wrong_creds()))
        );
        let salted = md5_hex(b"ps");
        assert_eq!(
            authenticate(&s, &creds("", "u", "", "t", "")),
            Err((41, "Token authentication requires salt.".to_string()))
        );
        assert!(authenticate(&s, &creds("", "u", "", &salted, "s")).is_ok());
    }
}
