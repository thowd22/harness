//! ID token verification: an RS256 JWT checked against the issuer's JWKS,
//! then `iss`, `aud`, `exp` and `nonce`.

use aws_lc_rs::signature::{RSA_PKCS1_2048_8192_SHA256, RsaPublicKeyComponents};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::Deserialize;
use serde_json::Value;

/// Clock skew allowed on `exp`, in seconds.
pub const LEEWAY_SECS: u64 = 60;

/// One JSON Web Key (only RSA keys are used).
#[derive(Debug, Clone, Deserialize)]
pub struct Jwk {
    /// Key type (`RSA`).
    pub kty: String,
    /// Key id.
    #[serde(default)]
    pub kid: Option<String>,
    /// Algorithm, if stated.
    #[serde(default)]
    pub alg: Option<String>,
    /// Modulus, base64url.
    #[serde(default)]
    pub n: Option<String>,
    /// Exponent, base64url.
    #[serde(default)]
    pub e: Option<String>,
}

/// A JWKS document.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Jwks {
    /// The keys.
    #[serde(default)]
    pub keys: Vec<Jwk>,
}

/// What a verified ID token says about the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdClaims {
    /// Account identity (`sub`).
    pub sub: String,
    /// Email address, if present.
    pub email: Option<String>,
    /// Expiry (Unix seconds).
    pub exp: u64,
}

/// What the ID token must match.
#[derive(Debug, Clone, Copy)]
pub struct Expected<'a> {
    /// `iss`.
    pub issuer: &'a str,
    /// `aud` (the issued client id).
    pub audience: &'a str,
    /// `nonce` of this attempt.
    pub nonce: &'a str,
    /// Current Unix time.
    pub now: u64,
}

fn b64(part: &str, what: &str) -> Result<Vec<u8>, String> {
    URL_SAFE_NO_PAD
        .decode(part.trim_end_matches('='))
        .map_err(|_| format!("{what} is not base64url"))
}

/// Verifies `token` and returns its claims. Errors are short and never
/// contain the token.
pub fn verify_id_token(token: &str, jwks: &Jwks, want: Expected<'_>) -> Result<IdClaims, String> {
    let parts: Vec<&str> = token.split('.').collect();
    let [h, p, s] = parts.as_slice() else {
        return Err("not a JWT".into());
    };
    let header: Value =
        serde_json::from_slice(&b64(h, "header")?).map_err(|_| "header is not JSON")?;
    let alg = header["alg"].as_str().unwrap_or("");
    if alg != "RS256" {
        return Err(format!(
            "unsupported signing algorithm `{alg}` (expected RS256)"
        ));
    }
    let kid = header["kid"].as_str();
    let key = jwks
        .keys
        .iter()
        .filter(|k| k.kty == "RSA" && k.alg.as_deref().is_none_or(|a| a == "RS256"))
        .find(|k| kid.is_none() || k.kid.as_deref() == kid)
        .ok_or_else(|| {
            format!(
                "no signing key `{}` in the issuer's JWKS",
                kid.unwrap_or("?")
            )
        })?;
    let (Some(n), Some(e)) = (&key.n, &key.e) else {
        return Err("JWKS key has no modulus or exponent".into());
    };
    let (n, e) = (b64(n, "modulus")?, b64(e, "exponent")?);
    let sig = b64(s, "signature")?;
    RsaPublicKeyComponents { n: &n, e: &e }
        .verify(
            &RSA_PKCS1_2048_8192_SHA256,
            format!("{h}.{p}").as_bytes(),
            &sig,
        )
        .map_err(|_| "bad signature")?;

    let claims: Value =
        serde_json::from_slice(&b64(p, "payload")?).map_err(|_| "payload is not JSON")?;
    let iss = claims["iss"].as_str().unwrap_or("");
    if iss.trim_end_matches('/') != want.issuer.trim_end_matches('/') {
        return Err(format!("unexpected issuer `{iss}`"));
    }
    let aud_ok = match &claims["aud"] {
        Value::String(a) => a == want.audience,
        Value::Array(a) => a.iter().any(|v| v.as_str() == Some(want.audience)),
        _ => false,
    };
    if !aud_ok {
        return Err("audience is not harness's client id".into());
    }
    let exp = claims["exp"].as_u64().ok_or("no expiry")?;
    if exp + LEEWAY_SECS < want.now {
        return Err("expired".into());
    }
    if claims["nonce"].as_str() != Some(want.nonce) {
        return Err("nonce does not match this sign-in".into());
    }
    let sub = claims["sub"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or("no subject")?;
    Ok(IdClaims {
        sub: sub.to_owned(),
        email: claims["email"].as_str().map(str::to_owned),
        exp,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chatgpt::mock::TestSigner;
    use serde_json::json;

    fn want(now: u64) -> Expected<'static> {
        Expected {
            issuer: "https://issuer.test",
            audience: "oaiapp_1",
            nonce: "n1",
            now,
        }
    }

    fn claims(exp: u64) -> Value {
        json!({"iss": "https://issuer.test/", "aud": ["oaiapp_1"], "exp": exp, "nonce": "n1",
               "sub": "user-1", "email": "a@example.com"})
    }

    #[test]
    fn accepts_a_valid_token_and_rejects_tampering() {
        let signer = TestSigner::new("k1");
        let jwks: Jwks = serde_json::from_value(signer.jwks()).unwrap();
        let t = signer.sign(&claims(2000));
        let c = verify_id_token(&t, &jwks, want(1000)).unwrap();
        assert_eq!(c.sub, "user-1");
        assert_eq!(c.email.as_deref(), Some("a@example.com"));

        let other = TestSigner::new("k1");
        let forged = other.sign(&claims(2000));
        assert_eq!(
            verify_id_token(&forged, &jwks, want(1000)).unwrap_err(),
            "bad signature"
        );
        let mut parts: Vec<String> = t.split('.').map(str::to_owned).collect();
        parts[1] = URL_SAFE_NO_PAD.encode(
            serde_json::to_vec(&json!({"iss": "https://issuer.test", "aud": "oaiapp_1", "exp": 9999, "nonce": "n1", "sub": "evil"})).unwrap(),
        );
        assert_eq!(
            verify_id_token(&parts.join("."), &jwks, want(1000)).unwrap_err(),
            "bad signature"
        );
    }

    #[test]
    fn checks_issuer_audience_expiry_and_nonce() {
        let signer = TestSigner::new("k1");
        let jwks: Jwks = serde_json::from_value(signer.jwks()).unwrap();
        let mut c = claims(2000);
        let sign = |c: &Value| signer.sign(c);
        assert!(
            verify_id_token(&sign(&c), &jwks, want(3000))
                .unwrap_err()
                .contains("expired")
        );
        c["nonce"] = json!("other");
        assert!(
            verify_id_token(&sign(&c), &jwks, want(1000))
                .unwrap_err()
                .contains("nonce")
        );
        c = claims(2000);
        c["aud"] = json!("someone-else");
        assert!(
            verify_id_token(&sign(&c), &jwks, want(1000))
                .unwrap_err()
                .contains("audience")
        );
        c = claims(2000);
        c["iss"] = json!("https://evil.test");
        assert!(
            verify_id_token(&sign(&c), &jwks, want(1000))
                .unwrap_err()
                .contains("issuer")
        );
        assert_eq!(
            verify_id_token("a.b", &jwks, want(1000)).unwrap_err(),
            "not a JWT"
        );
        let none_alg = format!(
            "{}.{}.",
            URL_SAFE_NO_PAD.encode(br#"{"alg":"none"}"#),
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims(2000)).unwrap())
        );
        assert!(
            verify_id_token(&none_alg, &jwks, want(1000))
                .unwrap_err()
                .contains("RS256")
        );
        let empty = Jwks::default();
        assert!(
            verify_id_token(&sign(&claims(2000)), &empty, want(1000))
                .unwrap_err()
                .contains("no signing key")
        );
    }
}
