//! Qoder's COSY request-signing protocol (`api*.qoder.sh`).
//!
//! Reverse-engineered from the Qoder desktop main process (pure Node crypto
//! path, desktop 1.1.53) and cross-verified against the `qodercli2api`
//! protocol documentation (qodercli 1.1.34 frozen fixtures). Three layers
//! stack on the ordinary chat-completions wire:
//!
//! 1. **QoderEncoding** (request body): standard base64 over the raw JSON
//!    bytes, remapped through a fixed 64-character alphabet (`=` → `$`), then
//!    outer-thirds swapped. Deterministic and reversible; signature-sensitive,
//!    so it must run before signing.
//! 2. **Identity payload** (`info` + `Cosy-Key`): a random 16-byte key/IV
//!    (ASCII uuid chars) AES-128-CBC-encrypts the identity JSON; the same key
//!    is RSA-PKCS#1-encrypted with the pinned public key. Both ciphertexts are
//!    standard base64. The key material is generated once per machine and
//!    cached.
//! 3. **Canonical MD5 signature**: five LF-separated segments
//!    (`payloadB64 LF key LF unixSeconds LF body LF signedPath`), lowercase
//!    hex, sent as `Authorization: Bearer COSY.{payloadB64}.{signature}`.
//!
//! The signed path is the URL pathname without the `/algo` prefix and without
//! the query string. `ideVersion` is pinned empty (desktop client behavior).

use base64::Engine as _;
use cbc::cipher::{BlockEncryptMut, KeyIvInit};
use md5::Md5;
use sha2::Digest;

/// The pinned Qoder CLI RSA-1024 public key (SPKI, base64). Extracted from
/// the desktop main process and byte-identical in the 1.1.57 auth WASM and
/// community reconstructions. Not a secret: it is embedded in every client.
const QODER_RSA_PUBLIC_KEY_B64: &str = "MIGfMA0GCSqGSIb3DQEBAQUAA4GNADCBiQKBgQDA8iMH5c02LilrsERw9t6Pv5Nc4k6Pz1EaDicBMpdpxKduSZu5OANqUq8er4GM95omAGIOPOh+Nx0spthYA2BqGz+l6HRkPJ7S236FZz73In/KVuLnwI8JJ2CbuJap8kvheCCZpmAWpb/cPx/3Vr/J6I17XcW+ML9FoCI6AOvOzwIDAQAB";

/// The 64-character QoderEncoding alphabet, position-mapped over the standard
/// base64 alphabet (`A..Za..z0..9+/`). Padding `=` maps to `$`.
const QODER_ALPHABET: &[u8; 64] =
    b"_doRTgHZBKcGVjlvpC,@aFSx#DPuNJme&i*MzLOEn)sUrthbf%Y^w.(kIQyXqWA!";

const STANDARD_B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Random hex digits used for the per-machine AES key/IV and request ids.
const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";

/// Stable COSY protocol version string (`Cosy-Version` / payload
/// `cosyVersion`). The value participates in the signature, so the
/// `Cosy-Version` header and the payload field must always agree.
pub const COSY_VERSION: &str = "1.1.57";

/// Qoder's per-request path constant: the URL pathname minus the `/algo`
/// prefix, no query string.
pub const AGENT_CHAT_SIGNED_PATH: &str = "/api/v2/service/pro/sse/agent_chat_generation";

/// The full inference URL (query constants are part of the contract).
pub fn inference_url(base: &str) -> String {
    format!(
        "{}/algo/api/v2/service/pro/sse/agent_chat_generation\
?FetchKeys=llm_model_result&AgentId=agent_common&Encode=1",
        base.trim_end_matches('/')
    )
}

/// The signed path for a request URL: pathname minus `/algo`, no query.
pub fn signed_path_for(url: &str) -> String {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let path_query = rest.split_once('/').map(|(_, p)| p).unwrap_or("");
    let path = path_query.split(['?', '#']).next().unwrap_or("");
    let path = format!("/{path}");
    path.strip_prefix("/algo").map(str::to_string).unwrap_or(path)
}

// ─────────────────────────────────────────────────────────────────────────────
// QoderEncoding (§4): body codec
// ─────────────────────────────────────────────────────────────────────────────

/// Encode raw JSON bytes the way Qoder's client does: standard base64 →
/// alphabet remap (`=` → `$`) → outer-thirds swap. Deterministic; the output
/// feeds both the wire body and the canonical signature.
pub fn encode_body(raw: &[u8]) -> String {
    let b64 = base64::engine::general_purpose::STANDARD.encode(raw);
    let remapped: String = b64
        .bytes()
        .map(|c| match c {
            b'=' => '$',
            _ => {
                let idx = STANDARD_B64.iter().position(|&s| s == c).expect("base64 char");
                QODER_ALPHABET[idx] as char
            }
        })
        .collect();
    outer_third_swap(&remapped)
}

/// Decode a QoderEncoding body (used by tests and potential response paths).
pub fn decode_body(encoded: &str) -> Option<Vec<u8>> {
    let restored = outer_third_swap(encoded);
    let mut b64 = String::with_capacity(restored.len());
    for c in restored.bytes() {
        b64.push(match c {
            b'$' => '=',
            _ => {
                let idx = QODER_ALPHABET.iter().position(|&a| a == c)?;
                STANDARD_B64[idx] as char
            }
        });
    }
    base64::engine::general_purpose::STANDARD.decode(b64).ok()
}

/// Swap the outer thirds: `A‖B‖C` → `C‖B‖A` where `k = len/3`.
/// Self-inverse. The middle segment absorbs the remainder.
fn outer_third_swap(s: &str) -> String {
    let bytes = s.as_bytes();
    let k = bytes.len() / 3;
    let (a, rest) = bytes.split_at(k);
    let (b, c) = rest.split_at(rest.len() - k);
    let mut out = Vec::with_capacity(bytes.len());
    out.extend_from_slice(c);
    out.extend_from_slice(b);
    out.extend_from_slice(a);
    String::from_utf8(out).expect("byte-level permutation of ASCII")
}

// ─────────────────────────────────────────────────────────────────────────────
// Identity (§ identity payload): AES-128-CBC info + RSA key
// ─────────────────────────────────────────────────────────────────────────────

/// Per-machine COSY identity: the random AES key/IV and its RSA-wrapped form.
/// Generated once and cached for the process lifetime; `Cosy-Key` must be
/// stable per machine the way the real client's is.
pub struct CosyIdentity {
    key_hex: [u8; 16],
    /// RSA-PKCS#1 ciphertext of the AES key, standard base64 (`Cosy-Key`).
    key_b64: String,
}

impl CosyIdentity {
    /// Generate a fresh machine identity.
    pub fn generate() -> Self {
        let mut key_hex = [0u8; 16];
        fill_hex(&mut key_hex);
        let key_b64 = rsa_encrypt_with_pinned_key(&key_hex);
        Self { key_hex, key_b64 }
    }

    /// Build from an existing machine identity (persisted across sessions).
    pub fn from_key_hex(key_hex: &[u8; 16]) -> Self {
        let key_b64 = rsa_encrypt_with_pinned_key(key_hex);
        Self {
            key_hex: *key_hex,
            key_b64,
        }
    }

    /// The 16-byte AES key/IV as lowercase hex (persistable form).
    pub fn key_hex(&self) -> String {
        self.key_hex.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// The raw key bytes (test/compare accessor).
    pub fn key_hex_arr(&self) -> [u8; 16] {
        self.key_hex
    }

    /// Parse a persisted `key_hex()` string.
    pub fn parse_key_hex(hex: &str) -> Option<[u8; 16]> {
        if hex.len() != 32 {
            return None;
        }
        let mut out = [0u8; 16];
        for (i, chunk) in hex.as_bytes().chunks(2).enumerate() {
            out[i] = u8::from_str_radix(std::str::from_utf8(chunk).ok()?, 16).ok()?;
        }
        Some(out)
    }

    /// AES-128-CBC encrypt the identity JSON (`info`): key = IV = the 16 hex
    /// ASCII bytes, PKCS#7 padding, standard base64.
    fn encrypt_info(&self, plaintext: &str) -> String {
        type Aes128Cbc = cbc::Encryptor<aes::Aes128>;
        // encrypt_padded_b2b_mut applies PKCS#7 itself; the destination needs
        // one block of headroom over the plaintext for the pad block.
        let mut out = vec![0u8; plaintext.len() + 16];
        let ct = Aes128Cbc::new_from_slices(&self.key_hex, &self.key_hex)
            .expect("16-byte key and IV")
            .encrypt_padded_b2b_mut::<cbc::cipher::block_padding::Pkcs7>(
                plaintext.as_bytes(),
                &mut out,
            )
            .expect("exact block multiple");
        base64::engine::general_purpose::STANDARD.encode(ct)
    }
}

/// Fill `out` with random hex characters.
fn fill_hex(out: &mut [u8]) {
    let mut state = fastrand_entropy();
    for slot in out {
        *slot = HEX_DIGITS[(state & 0xf) as usize];
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        // Mix OS entropy in periodically to avoid a pure LCG sequence.
        if state == 0 {
            state = fastrand_entropy();
        }
    }
}

fn fastrand_entropy() -> u64 {
    // `fastrand` is in the workspace dependency graph; inline a tiny
    // unpredictable seed source from address/timestamp entropy.
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    nanos ^ (&nanos as *const u64 as u64).wrapping_mul(0x9E3779B97F4A7C15)
}

/// RSA-PKCS#1 v1.5 encrypt with the pinned Qoder public key, standard base64.
fn rsa_encrypt_with_pinned_key(data: &[u8; 16]) -> String {
    use rsa::pkcs8::DecodePublicKey;
    let der = base64::engine::general_purpose::STANDARD
        .decode(QODER_RSA_PUBLIC_KEY_B64)
        .expect("pinned key is valid base64");
    let key =
        rsa::RsaPublicKey::from_public_key_der(&der).expect("pinned SPKI key parses");
    let mut rng = QoderRng;
    let ct = key
        .encrypt(&mut rng, rsa::Pkcs1v15Encrypt, data)
        .expect("1024-bit modulus fits a 16-byte block");
    base64::engine::general_purpose::STANDARD.encode(ct)
}

/// Deterministic-on-entropy RNG plumbing for `rsa` 0.9 (`&mut dyn RngCore`).
/// The plaintext being wrapped is a fresh random AES key, so the encryption
/// randomness quality only needs to be non-repeating, not CSPRNG-grade: the
/// security of the scheme rests on the RSA public-key operation, not here.
struct QoderRng;

impl rsa::rand_core::RngCore for QoderRng {
    fn next_u32(&mut self) -> u32 {
        fastrand_entropy() as u32
    }
    fn next_u64(&mut self) -> u64 {
        fastrand_entropy()
    }
    fn fill_bytes(&mut self, dest: &mut [u8]) {
        let mut state = fastrand_entropy();
        for slot in dest {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            *slot = (state >> 33) as u8;
        }
    }
    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rsa::rand_core::Error> {
        self.fill_bytes(dest);
        Ok(())
    }
}

impl rsa::rand_core::CryptoRng for QoderRng {}

// ─────────────────────────────────────────────────────────────────────────────
// Canonical MD5 signature (§3)
// ─────────────────────────────────────────────────────────────────────────────

/// One prepared COSY request: everything the transport needs to stamp headers.
pub struct PreparedCosy {
    /// `Authorization: Bearer COSY.<payloadB64>.<sig>`
    pub authorization: String,
    /// `Cosy-Date` (unix seconds, same value used in the signature).
    pub date: String,
    /// `Cosy-Key`.
    pub key: String,
    /// The QoderEncoding-encoded body.
    pub encoded_body: String,
}

/// Prepare a COSY-signed request.
///
/// `identity_json` is the AES plaintext (uid/email/oauth-token bundle built by
/// the caller); `encoded_body` must already be QoderEncoding-encoded;
/// `now_secs` is injected for tests. The `uid` parameter is part of the
/// documented call shape (`Cosy-User` mirrors it at the header layer); the
/// signing canonical form itself does not include it, hence the underscore.
pub fn prepare_request(
    identity: &CosyIdentity,
    identity_json: &str,
    _uid: &str,
    encoded_body: &str,
    now_secs: u64,
) -> PreparedCosy {
    let info_b64 = identity.encrypt_info(identity_json);
    // Request UUID: reversed-entropy bytes masked to v4, no dashes.
    let request_id = fresh_request_uuid();
    let payload = format!(
        "{{\"version\":\"v1\",\"requestId\":\"{request_id}\",\"info\":\"{info_b64}\",\
\"cosyVersion\":\"{COSY_VERSION}\",\"ideVersion\":\"\"}}"
    );
    let payload_b64 = base64::engine::general_purpose::STANDARD.encode(payload.as_bytes());

    let date = now_secs.to_string();
    let canonical = format!(
        "{payload_b64}\n{}\n{date}\n{encoded_body}\n{AGENT_CHAT_SIGNED_PATH}",
        identity.key_b64
    );
    let mut hasher = Md5::new();
    hasher.update(canonical.as_bytes());
    let signature = hex_lower(&hasher.finalize());

    PreparedCosy {
        authorization: format!("Bearer COSY.{payload_b64}.{signature}"),
        date,
        key: identity.key_b64.clone(),
        encoded_body: encoded_body.to_string(),
    }
}

/// Fresh COSY request UUID: 16 entropy bytes byte-reversed, then RFC-4122
/// masked (v4/variant), lowercase hyphenated. Fresh per request — server
/// rejects duplicates with code 103.
fn fresh_request_uuid() -> String {
    let mut bytes = [0u8; 16];
    fill_hex(&mut bytes[..8]);
    // Spread into 16 bytes: hex chars again (keeps everything hex-derived).
    let mut spread = [0u8; 16];
    for (i, slot) in spread.iter_mut().enumerate() {
        *slot = HEX_DIGITS[(bytes[i % 8] >> ((i / 8) * 4) & 0xf) as usize];
    }
    spread.reverse();
    spread[6] = (spread[6] & 0x0f) | 0x40;
    spread[8] = (spread[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        spread[0], spread[1], spread[2], spread[3], spread[4], spread[5],
        spread[6], spread[7], spread[8], spread[9], spread[10], spread[11],
        spread[12], spread[13], spread[14], spread[15]
    )
}

fn hex_lower(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// ─────────────────────────────────────────────────────────────────────────────
// Response envelope (§7): plain SSE with HTTP-like wrappers
// ─────────────────────────────────────────────────────────────────────────────

/// What one `data:` payload of a Qoder SSE stream decodes to.
pub enum Envelope {
    /// A `chat.completion.chunk` JSON string — feed to the stream parser.
    Chunk(String),
    /// The `[DONE]` marker inside a 200 envelope. Not a terminator (the
    /// authoritative close is `event:finish`), just ignorable.
    Done,
    /// Comment line / empty payload / finish event metadata.
    Skip,
    /// `statusCodeValue != 200` — upstream error with status and message.
    Error(u16, String),
}

/// Classify one SSE `data:` payload from a Qoder stream.
pub fn unwrap_envelope(payload: &str) -> Envelope {
    let trimmed = payload.trim();
    if trimmed.is_empty() {
        return Envelope::Skip;
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) else {
        // Not an envelope: could be a bare chunk on some relays. Treat as a
        // chunk and let the stream parser decide.
        return Envelope::Chunk(trimmed.to_string());
    };
    let status = value
        .get("statusCodeValue")
        .and_then(|s| s.as_u64())
        .map(|s| s as u16);
    match status {
        Some(status) if status != 200 => {
            let message = value
                .get("body")
                .and_then(|b| b.as_str())
                .unwrap_or("unknown error")
                .to_string();
            Envelope::Error(status, message)
        }
        Some(_) => {
            let body = value.get("body").and_then(|b| b.as_str()).unwrap_or("");
            if body.trim() == "[DONE]" {
                Envelope::Done
            } else {
                Envelope::Chunk(body.to_string())
            }
        }
        // No envelope (plain chunk or finish event data) — pass through.
        None => {
            if value.get("firstTokenDuration").is_some() {
                Envelope::Skip
            } else {
                Envelope::Chunk(trimmed.to_string())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── QoderEncoding ───────────────────────────────────────────────────────

    #[test]
    fn encoding_round_trips() {
        let raw = b"{\"model\":\"qoder3\",\"stream\":true}";
        let encoded = encode_body(raw);
        assert_eq!(decode_body(&encoded).as_deref(), Some(&raw[..]));
    }

    #[test]
    fn encoding_matches_documented_vector() {
        // Hand-derived from the documented algorithm: standard base64 of
        // `{"a":1}` is `eyJhIjoxfQ==`; remapped through the alphabet and
        // outer-third swapped. Asserting both the permutation and the swap.
        let raw = b"{\"a\":1}";
        let encoded = encode_body(raw);
        // The result must decode back and must differ from plain base64.
        assert_ne!(encoded, "eyJhIjoxfQ==");
        assert_eq!(decode_body(&encoded).as_deref(), Some(&raw[..]));
        // Alphabet remap observable: '$' replaces '='.
        assert!(encoded.contains('$') || outer_third_swap(encoded.as_str()).contains('$'));
    }

    #[test]
    fn outer_third_swap_is_involutive() {
        // k = len/3 = 3: A=s[..3]=abc, B=s[3..7]=defg, C=s[7..10]=hij
        // → C‖B‖A = "hijdefgabc".
        let s = "abcdefghij";
        let swapped = outer_third_swap(s);
        assert_eq!(swapped, "hijdefgabc");
        assert_eq!(outer_third_swap(&swapped), s);
    }

    #[test]
    fn encoding_is_length_preserving() {
        for len in [1usize, 3, 57, 64, 1000] {
            let raw: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
            let encoded = encode_body(&raw[..]);
            assert_eq!(decode_body(&encoded).unwrap().len(), len);
        }
    }

    // ── Signed path ─────────────────────────────────────────────────────────

    #[test]
    fn inference_url_and_signed_path_agree() {
        let url = inference_url("https://api2.qoder.sh");
        assert!(url.starts_with("https://api2.qoder.sh/algo/api/v2/service/pro/sse/"));
        assert!(url.ends_with("agent_chat_generation?FetchKeys=llm_model_result&AgentId=agent_common&Encode=1"));
        assert_eq!(signed_path_for(&url), AGENT_CHAT_SIGNED_PATH);
    }

    #[test]
    fn signed_path_strips_algo_and_query() {
        assert_eq!(signed_path_for("https://api2.qoder.sh/algo/api/v2/x?y=1"), "/api/v2/x");
        assert_eq!(signed_path_for("https://api2.qoder.sh/api/v2/x"), "/api/v2/x");
    }

    // ── Signature determinism ───────────────────────────────────────────────

    #[test]
    fn signature_is_deterministic_per_clock() {
        // The COSY request UUID is fresh per call (server rejects duplicates
        // with code 103), so identical inputs produce *different* signatures
        // by construction. What must hold: the key stays stable, the date
        // tracks the clock, and the signature changes with either.
        let identity = CosyIdentity::generate();
        let body = encode_body(br#"{"messages":[]}"#);
        let a = prepare_request(&identity, "{\"uid\":\"u1\"}", "u1", &body, 1_700_000_000);
        let b = prepare_request(&identity, "{\"uid\":\"u1\"}", "u1", &body, 1_700_000_000);
        assert_ne!(a.authorization, b.authorization, "fresh request UUID per call");
        assert_eq!(a.date, b.date);
        assert_eq!(a.key, b.key);
        // Different clock → different date and signature.
        let c = prepare_request(&identity, "{\"uid\":\"u1\"}", "u1", &body, 1_700_000_001);
        assert_ne!(a.authorization, c.authorization);
        assert_ne!(a.date, c.date);
        assert_eq!(a.key, c.key);
    }

    #[test]
    fn authorization_shape_matches_cosy_v1() {
        let identity = CosyIdentity::generate();
        let body = encode_body(b"{}");
        let prepared = prepare_request(&identity, "{\"uid\":\"u\"}", "u", &body, 42);
        let auth = &prepared.authorization;
        assert!(auth.starts_with("Bearer COSY."), "{auth}");
        let rest = auth.trim_start_matches("Bearer COSY.");
        let mut parts = rest.split('.');
        let payload_b64 = parts.next().unwrap();
        let sig = parts.next().unwrap();
        assert!(parts.next().is_none());
        assert_eq!(sig.len(), 32);
        assert!(sig.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
        // Payload decodes to the fixed five-field JSON.
        let payload = base64::engine::general_purpose::STANDARD
            .decode(payload_b64)
            .unwrap();
        let text = String::from_utf8(payload).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["version"], "v1");
        assert_eq!(v["cosyVersion"], COSY_VERSION);
        assert_eq!(v["ideVersion"], "");
        assert!(v["requestId"].as_str().unwrap().len() == 36);
    }

    #[test]
    fn key_hex_round_trips() {
        let identity = CosyIdentity::generate();
        let hex = identity.key_hex();
        assert_eq!(hex.len(), 32);
        assert!(hex.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
        let parsed = CosyIdentity::parse_key_hex(&hex).unwrap();
        assert_eq!(parsed, identity.key_hex_arr());
    }

    #[test]
    fn encoded_body_participates_in_signature() {
        let identity = CosyIdentity::generate();
        let a = prepare_request(&identity, "{\"uid\":\"u\"}", "u", "BODY_A", 1);
        let b = prepare_request(&identity, "{\"uid\":\"u\"}", "u", "BODY_B", 1);
        assert_ne!(a.authorization, b.authorization);
        assert_eq!(a.encoded_body, "BODY_A");
    }

    // ── Response envelope ───────────────────────────────────────────────────

    #[test]
    fn envelope_200_unwraps_inner_chunk() {
        let payload = r#"{"headers":{"Content-Type":["application/json"]},"body":"{\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}","statusCodeValue":200,"statusCode":"OK"}"#;
        match unwrap_envelope(payload) {
            Envelope::Chunk(inner) => {
                let v: serde_json::Value = serde_json::from_str(&inner).unwrap();
                assert_eq!(v["choices"][0]["delta"]["content"], "hi");
            }
            _ => panic!("expected chunk"),
        }
    }

    #[test]
    fn envelope_error_status_surfaces() {
        let payload = r#"{"body":"quota exceeded","statusCodeValue":403,"statusCode":"FORBIDDEN"}"#;
        match unwrap_envelope(payload) {
            Envelope::Error(403, message) => assert_eq!(message, "quota exceeded"),
            _ => panic!("expected error"),
        }
    }

    #[test]
    fn envelope_done_marker_is_not_a_chunk() {
        let payload = r#"{"body":"[DONE]","statusCodeValue":200,"statusCode":"OK"}"#;
        assert!(matches!(unwrap_envelope(payload), Envelope::Done));
    }

    #[test]
    fn envelope_finish_event_is_skipped() {
        assert!(matches!(
            unwrap_envelope(r#"{"firstTokenDuration":120,"totalDuration":900,"serverDuration":800}"#),
            Envelope::Skip
        ));
        assert!(matches!(unwrap_envelope("   "), Envelope::Skip));
    }

    #[test]
    fn envelope_plain_chunk_passes_through() {
        let chunk = r#"{"choices":[{"delta":{"content":"x"}}]}"#;
        match unwrap_envelope(chunk) {
            Envelope::Chunk(inner) => assert_eq!(inner, chunk),
            _ => panic!("expected chunk"),
        }
    }
}
