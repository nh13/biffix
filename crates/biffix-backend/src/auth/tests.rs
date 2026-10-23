//! The production verifier (`CognitoJwtVerifier`) against a local JWKS server, with RSA keys
//! generated at test time: claim checks, `kid` selection and rotation, and the refresh cooldown.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;
use axum::Router;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use jsonwebtoken::{encode, EncodingKey, Header};
use rsa::pkcs1::EncodeRsaPrivateKey;
use rsa::traits::PublicKeyParts;
use serde_json::{json, Value};

use super::*;

const WEB: &str = "web-client";
const CLI: &str = "cli-client";

/// A test signing key: its private half as a jsonwebtoken key, its public half as a JWK.
struct TestKey {
    kid: &'static str,
    encoding: EncodingKey,
    jwk: Value,
}

fn make_key(kid: &'static str) -> TestKey {
    let private = rsa::RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048).unwrap();
    let der = private.to_pkcs1_der().unwrap();
    TestKey {
        kid,
        encoding: EncodingKey::from_rsa_der(der.as_bytes()),
        jwk: rsa_jwk(kid, &private),
    }
}

fn rsa_jwk(kid: &str, private: &rsa::RsaPrivateKey) -> Value {
    let public = private.to_public_key();
    json!({
        "kty": "RSA", "kid": kid, "use": "sig", "alg": "RS256",
        "n": URL_SAFE_NO_PAD.encode(public.n().to_bytes_be()),
        "e": URL_SAFE_NO_PAD.encode(public.e().to_bytes_be()),
    })
}

/// A fresh `bits`-bit RS256 key's JWK and a correctly signed token for `claims`. jsonwebtoken
/// refuses to sign with a key under 2048 bits, so the RS256 signature (PKCS#1 v1.5 over SHA-256)
/// is built here.
fn hand_signed_token(kid: &str, bits: usize, claims: &Value) -> (Value, String) {
    use sha2::Digest;
    // The DER DigestInfo prefix for SHA-256 (RFC 8017 section 9.2).
    const SHA256_PREFIX: [u8; 19] = [
        0x30, 0x31, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x01,
        0x05, 0x00, 0x04, 0x20,
    ];
    let private = rsa::RsaPrivateKey::new(&mut rsa::rand_core::OsRng, bits).unwrap();
    let header = URL_SAFE_NO_PAD.encode(json!({"alg": "RS256", "kid": kid}).to_string());
    let payload = URL_SAFE_NO_PAD.encode(claims.to_string());
    let message = format!("{header}.{payload}");
    let digest_info = [&SHA256_PREFIX[..], &sha2::Sha256::digest(message.as_bytes())].concat();
    let signature = private.sign(rsa::Pkcs1v15Sign::new_unprefixed(), &digest_info).unwrap();
    let token = format!("{message}.{}", URL_SAFE_NO_PAD.encode(signature));
    (rsa_jwk(kid, &private), token)
}

/// Two keys, generated once per test binary (RSA key generation is slow in a debug build).
fn keys() -> &'static (TestKey, TestKey) {
    static KEYS: OnceLock<(TestKey, TestKey)> = OnceLock::new();
    KEYS.get_or_init(|| (make_key("k1"), make_key("k2")))
}

/// What the fake JWKS endpoint serves, and how many times it has been fetched.
struct Jwks {
    status: StdMutex<StatusCode>,
    body: StdMutex<Value>,
    delay: StdMutex<Duration>,
    fetches: AtomicUsize,
}

impl Jwks {
    fn serve(&self, keys: &[&Value]) {
        *self.body.lock().unwrap() = json!({ "keys": keys });
        *self.status.lock().unwrap() = StatusCode::OK;
    }
    fn fail(&self) {
        *self.status.lock().unwrap() = StatusCode::INTERNAL_SERVER_ERROR;
    }
    fn fetches(&self) -> usize {
        self.fetches.load(Ordering::SeqCst)
    }
}

async fn jwks_handler(State(jwks): State<Arc<Jwks>>) -> (StatusCode, String) {
    jwks.fetches.fetch_add(1, Ordering::SeqCst);
    let delay = *jwks.delay.lock().unwrap();
    if !delay.is_zero() {
        tokio::time::sleep(delay).await;
    }
    let status = *jwks.status.lock().unwrap();
    let body = jwks.body.lock().unwrap().to_string();
    (status, body)
}

/// Start a JWKS server serving `initial`; returns the issuer URL it answers for.
async fn jwks_server(initial: &[&Value]) -> (String, Arc<Jwks>) {
    let jwks = Arc::new(Jwks {
        status: StdMutex::new(StatusCode::OK),
        body: StdMutex::new(json!({ "keys": initial })),
        delay: StdMutex::new(Duration::ZERO),
        fetches: AtomicUsize::new(0),
    });
    let app = Router::new()
        .route("/pool/.well-known/jwks.json", get(jwks_handler))
        .with_state(jwks.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}/pool"), jwks)
}

fn now() -> usize {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as usize
}

fn access_claims(issuer: &str, client: &str) -> Value {
    json!({
        "sub": "user-1", "iss": issuer, "client_id": client, "token_use": "access",
        "cognito:groups": ["biffix-viewers"], "username": "u1", "exp": now() + 3600,
    })
}

fn sign(key: &TestKey, claims: &Value) -> String {
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some(key.kid.to_string());
    encode(&header, claims, &key.encoding).unwrap()
}

fn audiences() -> Vec<String> {
    vec![WEB.to_string(), CLI.to_string()]
}

async fn verifier(issuer: &str, cooldown: Duration) -> CognitoJwtVerifier {
    CognitoJwtVerifier::load(issuer, cooldown).await.unwrap()
}

#[tokio::test]
async fn accepts_an_access_token_for_any_accepted_client() {
    let (k1, _) = keys();
    let (issuer, jwks) = jwks_server(&[&k1.jwk]).await;
    let v = verifier(&issuer, JWKS_REFRESH_COOLDOWN).await;
    for client in [WEB, CLI] {
        let claims = v
            .verify(&sign(k1, &access_claims(&issuer, client)), &issuer, &audiences())
            .await
            .unwrap();
        assert_eq!(claims.sub, "user-1");
        assert_eq!(claims.client_id.as_deref(), Some(client));
        assert_eq!(claims.groups, vec!["biffix-viewers"]);
    }
    assert_eq!(jwks.fetches(), 1, "a known kid never refetches");
}

#[tokio::test]
async fn rejects_tokens_that_are_not_this_pools_access_tokens() {
    let (k1, _) = keys();
    let (issuer, _jwks) = jwks_server(&[&k1.jwk]).await;
    let v = verifier(&issuer, JWKS_REFRESH_COOLDOWN).await;
    let base = access_claims(&issuer, WEB);
    let with = |key: &str, value: Value| {
        let mut claims = base.clone();
        claims[key] = value;
        claims
    };
    let without = |key: &str| {
        let mut claims = base.clone();
        claims.as_object_mut().unwrap().remove(key);
        claims
    };
    let cases = [
        ("an ID token", with("token_use", json!("id"))),
        ("no token_use", without("token_use")),
        ("a foreign client", with("client_id", json!("someone-else"))),
        ("no client_id", without("client_id")),
        ("another issuer", with("iss", json!("https://evil.example/pool"))),
        ("no iss", without("iss")),
        ("an expired token", with("exp", json!(now() - 3600))),
    ];
    for (what, claims) in cases {
        assert_eq!(
            v.verify(&sign(k1, &claims), &issuer, &audiences()).await,
            Err(AuthError::InvalidToken),
            "{what} must be rejected"
        );
    }
    // A verifier never checks `iss` against an issuer other than the pool its keys came from.
    let token = sign(k1, &access_claims("https://other/pool", WEB));
    assert_eq!(
        v.verify(&token, "https://other/pool", &audiences()).await,
        Err(AuthError::InvalidToken)
    );
}

#[tokio::test]
async fn rejects_a_non_rs256_or_kid_less_or_tampered_token() {
    let (k1, _) = keys();
    let (issuer, _jwks) = jwks_server(&[&k1.jwk]).await;
    let v = verifier(&issuer, JWKS_REFRESH_COOLDOWN).await;
    let claims = access_claims(&issuer, WEB);

    let mut hs = Header::new(Algorithm::HS256);
    hs.kid = Some("k1".into());
    let hmac = encode(&hs, &claims, &EncodingKey::from_secret(b"secret")).unwrap();
    assert_eq!(v.verify(&hmac, &issuer, &audiences()).await, Err(AuthError::InvalidToken));

    let no_kid = encode(&Header::new(Algorithm::RS256), &claims, &k1.encoding).unwrap();
    assert_eq!(v.verify(&no_kid, &issuer, &audiences()).await, Err(AuthError::MissingKeyId));

    let mut tampered = sign(k1, &claims);
    tampered.push('x');
    assert_eq!(v.verify(&tampered, &issuer, &audiences()).await, Err(AuthError::InvalidToken));
    assert_eq!(
        JwtVerifier::Cognito(v).verify(&sign(k1, &claims), &issuer, &[]).await,
        Err(AuthError::InvalidToken),
        "an empty audience set accepts nothing"
    );
}

/// Pins the jsonwebtoken semantics the verifier relies on, so a library upgrade that changes them
/// fails here: the 60 s `exp` leeway, a required and numeric `exp`, RS256 only (not another RSA
/// algorithm over the same key, nor `alg: none`), and the 2048-bit RSA minimum.
#[tokio::test]
async fn library_verification_semantics_are_pinned() {
    let (k1, _) = keys();
    let (issuer, jwks) = jwks_server(&[&k1.jwk]).await;
    let base = access_claims(&issuer, WEB);
    let (small_jwk, small_token) = hand_signed_token("small", 1024, &base);
    let (big_jwk, big_token) = hand_signed_token("big", 2048, &base);
    jwks.serve(&[&k1.jwk, &small_jwk, &big_jwk]);
    let v = verifier(&issuer, JWKS_REFRESH_COOLDOWN).await;
    assert!(
        v.verify(&big_token, &issuer, &audiences()).await.is_ok(),
        "the hand-built RS256 signature verifies with a 2048-bit key"
    );
    let with = |key: &str, value: Value| {
        let mut claims = base.clone();
        claims[key] = value;
        claims
    };

    let within_leeway = sign(k1, &with("exp", json!(now() - 30)));
    assert!(
        v.verify(&within_leeway, &issuer, &audiences()).await.is_ok(),
        "a token expired 30 s ago is inside the 60 s leeway"
    );
    let mut no_exp = base.clone();
    no_exp.as_object_mut().unwrap().remove("exp");
    let rejected = [
        ("expired past the leeway", sign(k1, &with("exp", json!(now() - 120)))),
        ("no exp", sign(k1, &no_exp)),
        ("a string exp", sign(k1, &with("exp", json!((now() + 3600).to_string())))),
        ("a key under 2048 bits", small_token),
    ];
    for (what, token) in rejected {
        assert_eq!(
            v.verify(&token, &issuer, &audiences()).await,
            Err(AuthError::InvalidToken),
            "{what} must be rejected"
        );
    }

    // Another RSA algorithm over the same key and kid.
    for alg in [Algorithm::RS512, Algorithm::PS256] {
        let mut header = Header::new(alg);
        header.kid = Some(k1.kid.to_string());
        let token = encode(&header, &base, &k1.encoding).unwrap();
        assert_eq!(
            v.verify(&token, &issuer, &audiences()).await,
            Err(AuthError::InvalidToken),
            "{alg:?} must be rejected"
        );
    }

    // `alg: none` with the right kid and an empty signature.
    let header = URL_SAFE_NO_PAD.encode(json!({"alg": "none", "kid": k1.kid}).to_string());
    let payload = URL_SAFE_NO_PAD.encode(base.to_string());
    assert_eq!(
        v.verify(&format!("{header}.{payload}."), &issuer, &audiences()).await,
        Err(AuthError::InvalidToken)
    );
}

#[tokio::test]
async fn an_unknown_kid_inside_the_cooldown_does_not_fetch() {
    let (k1, k2) = keys();
    let (issuer, jwks) = jwks_server(&[&k1.jwk]).await;
    let v = verifier(&issuer, JWKS_REFRESH_COOLDOWN).await;
    // The server now has k2, but the load just happened: the cooldown forbids a refetch.
    jwks.serve(&[&k1.jwk, &k2.jwk]);
    let token = sign(k2, &access_claims(&issuer, WEB));
    for _ in 0..5 {
        assert_eq!(v.verify(&token, &issuer, &audiences()).await, Err(AuthError::UnknownKeyId));
    }
    assert_eq!(jwks.fetches(), 1);
}

#[tokio::test]
async fn a_rotated_key_is_picked_up_by_one_refresh() {
    let (k1, k2) = keys();
    let (issuer, jwks) = jwks_server(&[&k1.jwk]).await;
    let v = verifier(&issuer, Duration::ZERO).await;
    jwks.serve(&[&k2.jwk]);
    let token = sign(k2, &access_claims(&issuer, WEB));
    assert!(v.verify(&token, &issuer, &audiences()).await.is_ok());
    assert!(v.verify(&token, &issuer, &audiences()).await.is_ok());
    assert_eq!(jwks.fetches(), 2, "one load + one refresh, then the kid is cached");
    // The refreshed set replaced the old one: k1 was rotated out.
    assert_eq!(
        v.verify(&sign(k1, &access_claims(&issuer, WEB)), &issuer, &audiences()).await,
        Err(AuthError::UnknownKeyId)
    );
}

#[tokio::test]
async fn a_failed_refresh_consumes_the_cooldown() {
    let (k1, k2) = keys();
    let (issuer, jwks) = jwks_server(&[&k1.jwk]).await;
    let v = verifier(&issuer, JWKS_REFRESH_COOLDOWN).await;
    *v.refresh.lock().await = LastRefresh::elapsed(); // the cooldown has elapsed
    jwks.fail();
    let token = sign(k2, &access_claims(&issuer, WEB));
    assert!(matches!(v.verify(&token, &issuer, &audiences()).await, Err(AuthError::Jwks(_))));
    // Still inside the window the failed attempt started: no second fetch, and the outage (a
    // 503), not an unknown key (a 401 the UI would answer by signing the user out, in a loop).
    for _ in 0..5 {
        assert!(matches!(v.verify(&token, &issuer, &audiences()).await, Err(AuthError::Jwks(_))));
    }
    assert_eq!(jwks.fetches(), 2);
    // The cached keys survive a failed refresh.
    assert!(v.verify(&sign(k1, &access_claims(&issuer, WEB)), &issuer, &audiences()).await.is_ok());
}

#[tokio::test]
async fn concurrent_unknown_kids_share_one_refresh() {
    let (k1, k2) = keys();
    let (issuer, jwks) = jwks_server(&[&k1.jwk]).await;
    let v = Arc::new(verifier(&issuer, JWKS_REFRESH_COOLDOWN).await);
    *v.refresh.lock().await = LastRefresh::elapsed();
    jwks.serve(&[&k1.jwk, &k2.jwk]);
    *jwks.delay.lock().unwrap() = Duration::from_millis(200);
    let token = sign(k2, &access_claims(&issuer, WEB));
    let calls = (0..20).map(|_| {
        let (v, token, issuer) = (v.clone(), token.clone(), issuer.clone());
        tokio::spawn(async move { v.verify(&token, &issuer, &audiences()).await })
    });
    for call in futures::future::join_all(calls).await {
        assert!(call.unwrap().is_ok(), "every caller sees the refreshed key");
    }
    assert_eq!(jwks.fetches(), 2, "one load + exactly one refresh");
}

#[tokio::test]
async fn keys_that_are_not_rs256_signing_keys_are_skipped() {
    let (k1, k2) = keys();
    let mut enc = k2.jwk.clone();
    enc["use"] = json!("enc");
    let unsupported = json!({ "kty": "XYZ", "kid": "weird" });
    let no_kid = json!({ "kty": "RSA", "n": k1.jwk["n"], "e": k1.jwk["e"] });
    let (issuer, _jwks) = jwks_server(&[&unsupported, &enc, &no_kid, &k1.jwk]).await;
    let v = verifier(&issuer, JWKS_REFRESH_COOLDOWN).await;
    assert!(v.verify(&sign(k1, &access_claims(&issuer, WEB)), &issuer, &audiences()).await.is_ok());
    assert_eq!(
        v.verify(&sign(k2, &access_claims(&issuer, WEB)), &issuer, &audiences()).await,
        Err(AuthError::UnknownKeyId),
        "an encryption key is never a signing key"
    );
}

#[tokio::test]
async fn an_rsa_signing_key_for_another_algorithm_is_skipped() {
    let (k1, k2) = keys();
    let mut rs512 = k2.jwk.clone();
    rs512["alg"] = json!("RS512");
    let (issuer, _jwks) = jwks_server(&[&rs512, &k1.jwk]).await;
    let v = verifier(&issuer, JWKS_REFRESH_COOLDOWN).await;
    assert_eq!(
        v.verify(&sign(k2, &access_claims(&issuer, WEB)), &issuer, &audiences()).await,
        Err(AuthError::UnknownKeyId),
        "only an RS256 key verifies"
    );
}

#[tokio::test]
async fn a_jwks_with_no_usable_key_fails_to_load() {
    let (_, k2) = keys();
    let mut enc = k2.jwk.clone();
    enc["use"] = json!("enc");
    let (issuer, jwks) = jwks_server(&[&enc]).await;
    assert!(matches!(
        CognitoJwtVerifier::load(&issuer, JWKS_REFRESH_COOLDOWN).await,
        Err(AuthError::Jwks(_))
    ));
    jwks.fail();
    assert!(matches!(
        CognitoJwtVerifier::load(&issuer, JWKS_REFRESH_COOLDOWN).await,
        Err(AuthError::Jwks(_))
    ));
}

#[test]
fn auth_mode_fails_closed_without_an_issuer() {
    // Each refusal is told apart by its kind: each needs a different fix from the operator.
    for (issuer, disabled) in [(None, None), (Some("  "), None), (None, Some("false"))] {
        assert!(
            matches!(
                AuthMode::from_env_values(issuer, disabled),
                Err(AuthModeError::Unauthenticated)
            ),
            "{issuer:?} {disabled:?}"
        );
    }
    assert!(matches!(
        AuthMode::from_env_values(None, Some("1")),
        Err(AuthModeError::NotBoolean(_))
    ));
    let err = AuthMode::from_env_values(None, Some("1")).unwrap_err().to_string();
    assert!(err.contains("BIFFIX_AUTH_DISABLED") && err.contains("true or false"), "{err}");
    assert_eq!(AuthMode::from_env_values(None, Some("true")).unwrap(), AuthMode::Disabled);
    assert_eq!(
        AuthMode::from_env_values(Some("https://idp/pool"), None).unwrap(),
        AuthMode::Cognito { issuer: "https://idp/pool".into() }
    );
    assert_eq!(
        AuthMode::from_env_values(Some("https://idp/pool"), Some("false")).unwrap(),
        AuthMode::Cognito { issuer: "https://idp/pool".into() }
    );
    assert!(matches!(
        AuthMode::from_env_values(Some("https://idp/pool"), Some("true")),
        Err(AuthModeError::Ambiguous)
    ));
}

#[test]
fn accepted_audiences_trim_dedupe_and_add_the_cli_client() {
    assert_eq!(accepted_audiences("web", Some("cli")), vec!["web", "cli"]);
    assert_eq!(accepted_audiences("web", Some("web")), vec!["web"]);
    assert_eq!(accepted_audiences(" a , ,b,a ", None), vec!["a", "b"]);
    assert_eq!(accepted_audiences("web", Some("  ")), vec!["web"]);
    assert!(accepted_audiences(" , ", None).is_empty());
}

#[tokio::test]
async fn a_jwks_outage_answers_503_not_401() {
    // A token signed by a key the cached JWKS lacks, while the JWKS cannot be fetched: the API's
    // outage, which must read "retry", not "sign in again" (a 401 would log a user out whose
    // token is fine).
    let (k1, k2) = keys();
    let (issuer, jwks) = jwks_server(&[&k1.jwk]).await;
    let v = verifier(&issuer, JWKS_REFRESH_COOLDOWN).await;
    *v.refresh.lock().await = LastRefresh::elapsed();
    jwks.fail();
    let auth = crate::AuthContext {
        verifier: JwtVerifier::Cognito(v),
        issuer: issuer.clone(),
        audiences: audiences(),
        rbac: Arc::new(crate::auth::rbac::Rbac::new(Default::default())),
    };
    let app = crate::api::router(crate::testing::TestState::new().with_auth(auth).build());
    let token = sign(k2, &access_claims(&issuer, WEB));
    let (status, _) = crate::testing::call_as(app, "GET", "/runs", None, Some(&token)).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
}
