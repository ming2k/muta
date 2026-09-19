# Qoder Provider Integration

Reverse-engineering notes, protocol reference, and maintenance know-how for
muta's Alibaba Qoder subscription integration (`qoder` connection template,
COSY-signed SSE inference). This page is the working knowledge base for
debugging the integration or re-verifying it against newer Qoder clients.

For the general subscription architecture, see
[OAuth2 subscription providers](oauth-subscription-providers.md).

---

## 1. What Qoder is and how muta classifies it

Qoder (qoder.com, international; qoder.com.cn, CN) is Alibaba's AI coding
IDE. Its CLI (`qodercli`) ships a subscription inference surface that is
OpenAI-compatible at the chunk level but wrapped in a proprietary signing
and encoding protocol:

- **Authentication**: device-authorization flow (Qoder-flavored PKCE) or a
  pasted personal-access token (`pt-…`).
- **Wire**: `POST …/algo/api/v2/service/pro/sse/agent_chat_generation` with
  a QoderEncoding-encoded body, a COSY-signed header set, and a plain-SSE
  response whose `data:` frames are HTTP-like envelopes.
- **muta dialect**: `Transport::OpenAi { dialect: Qoder }` — the standard
  chat-completions wire with three Qoder-specific layers stacked on top
  (body codec, header signature, response envelope).

Qoder publishes no live model list and no public API specification. The
protocol below was reconstructed from the shipped client binaries.

---

## 2. Reverse-engineering provenance (how we know this)

The protocol is corroborated by three independent sources. When Qoder ships
a new client, re-checking these sources in order is the cheapest path to a
diff.

| Source | What it gave us | Where |
|---|---|---|
| **Desktop client** (`Qoder-linux-amd64.deb`, main process `app.asar → /out/main/index.js`) | The original pure-Node-crypto COSY signing path (RSA/AES via Node built-ins), pinned RSA-1024 public key, OAuth client ids for both regions | `/tmp/qoder-re/asar-out/index.js` (offset ≈ 3.91 MB) |
| **Embedded CLI runtime** (`resources/app.asar.unpacked/…/qoder-agent-sdk/dist/_worker/qoder-worker-runtime.obf.mjs`, ~33 MB) | Device-flow parameters, client ids (obfuscated with `base64 + XOR("wAhs4UljGP3g")`), embedded `qoder_auth_wasm` | unpacked from the same deb |
| **`Liki4/qodercli2api`** (community) | Full protocol documentation for qodercli 1.1.34: `docs/oauth.md`, `docs/inference-protocol.md`, frozen fixtures, 217 passing Go tests | `/tmp/qoder-re/ref-liki4/` |
| **Local `qodercli` 1.1.57 binary** (Bun v1.4.2 single-file, 185 MB, not stripped) | The 1.1.57 auth WASM (carved from a base64 blob in the module band, 298,606 bytes), the `Cosy-ClientIp` header addition, an embedded 28-uid feature whitelist | carved to `/tmp/qoder-re/qoder_auth_wasm_1.1.57.wasm` |

Cross-validation rule: a constant counts as confirmed only when at least two
of these sources agree. Everything below passed that test except where
marked otherwise.

### Carving the auth WASM from a Bun binary (repeatable recipe)

Bun-compiled CLIs embed the JS bundle and assets as base64 in the binary.
To extract the signing module from a future version:

1. Locate the bundle region: `grep -abo '// @bun'` and scan for large
   base64 runs (`[A-Za-z0-9+/]{50000,}`) in the 87–95 MB band.
2. Find a run starting with `AGFzbQ` (the WASM magic `\0asm` in base64).
3. Decode with padding fix-up: pad the run to a multiple of 4 before
   decoding; verify `raw[:4] == b'\x00asm\x01\x00\x00\x00'`.
4. Sanity-check the module by walking sections (type/import/export/code)
   and searching the data section for `Bearer COSY.`, `Cosy-`, and the
   identity-payload field names.

---

## 3. Protocol reference

### 3.1 Endpoints

| Surface | International | CN |
|---|---|---|
| Inference (COSY SSE) | `https://api1/api2/api3.qoder.sh` (default `api2`) | `https://gateway.qoder.com.cn` |
| OpenAPI (token, userinfo) | `https://openapi.qoder.sh` | `https://openapi.qoder.com.cn` |
| Device-flow page | `https://qoder.com/device/selectAccounts` | `https://qoder.com.cn/...` |
| Daily/test (seen in client) | `daily-openapi.qoder.sh`, `test-openapi.qoder.sh` | — |

Inference URL (query constants are part of the contract):

```text
POST {base}/algo/api/v2/service/pro/sse/agent_chat_generation
    ?FetchKeys=llm_model_result&AgentId=agent_common&Encode=1
```

The signed path is the URL pathname **without** the `/algo` prefix and
without the query: `/api/v2/service/pro/sse/agent_chat_generation`.

### 3.2 Authentication

**Device flow** (default for OAuth connections):

1. Client generates `verifier` (43–128 chars), `challenge =
   base64url-no-pad(SHA256(verifier))`, `nonce` (uuid), `machine_id`
   (36-char UUID, persisted — see §5), `client_id`.
2. Browser opens
   `https://qoder.com/device/selectAccounts?challenge=…&challenge_method=S256&nonce=…&machine_id=…&client_id=…`
   — there is no device-code request step; the browser URL is the device
   code.
3. Poll `GET https://openapi.qoder.sh/api/v1/deviceToken/poll?nonce=…&verifier=…&challenge_method=S256`
   at 1 s. `404` = pending; `200` = the `dt-` device token (~30-day
   lifetime) with a `jrt-` refresh token. Deadline 300 s.
4. Inference consumes a `jt-` inference token via
   `POST /api/v1/jobToken/exchange {"personal_token": <dt- or pt->}`.

**PAT path** (supported as ApiKey-style auth): a pasted `pt-…` token is
used directly as the COSY `security_oauth_token` — no exchange is required
on the verified path (community-verified via the Sliverkiss
`cpa-plugin` KNOWNLEDGE notes; muta implements this).

OAuth client ids (public constants, embedded in every client):

```text
international: e93fe488-5778-4c35-a6fc-0f54ed7b3139
CN:            e883ade2-e6e3-4d6d-adf7-f92ceff5fdcb
```

(Extracted by decoding the obfuscated `FZa`/`UZa` constants with the
`_$d` helper: standard base64 then XOR with the repeating key
`wAhs4UljGP3g`.)

### 3.3 Request identity (typed, provider-owned)

Qoder's identity material lives in `muta_contracts::QoderRequestIdentity`,
attached to `ResolvedAuth.qoder` — deliberately **not** reusing the
ChatGPT `account_id` or Google `project_id` fields. One instance per
connection, persisted in the auth store as `TokenSet.qoder`
(`QoderStoredIdentity`, serde-defaulted so older `auth.toml` files keep
decoding).

| Field | Feeds | Persistence rule |
|---|---|---|
| `uid` | `Cosy-User` header + payload `uid` | from the device-token/exchange response |
| `machine_key_hex` | AES-128 key/IV (16 bytes) + `Cosy-Key` (RSA-wrapped) | generated once per device, never rotated (rotating reads as device churn to the risk layer) |
| `data_policy_agreed` | `Cosy-Data-Policy` (`agree`/`disagree`) | set at login |
| `organization_id` | `Cosy-Organization-Id` (omitted when empty) | from userinfo, when the account has one |
| `organization_tags` | `Cosy-Organization-Tags` (comma-joined, order preserved) | from userinfo |

The machine key also exists in a standalone persisted file
(`state_dir/machine_id`) for the device-flow correlation id.

### 3.4 QoderEncoding (request body)

Deterministic, reversible, signature-sensitive. Applied to the raw
chat-completions JSON bytes:

1. Standard padded base64 (`+/`, `=` padding).
2. Remap the 64-char alphabet by position to:
   ```text
   _doRTgHZBKcGVjlvpC,@aFSx#DPuNJme&i*MzLOEn)sUrthbf%Y^w.(kIQyXqWA!
   ```
   `=` → `$`.
3. Outer-thirds swap: `k = len/3`, output `C‖B‖A` (middle absorbs the
   remainder). Self-inverse.

Implementation: `muta-llm-client/src/protocol/openai/chat_completions/qoder.rs`
(`encode_body` / `decode_body` / `outer_third_swap`).

### 3.5 COSY signature

Identity layer (per request, inputs cached):

- Identity payload (AES-128-CBC plaintext, PKCS#7, key = IV = the 16
  machine-key bytes):
  ```json
  {"uid":"<uid>","aid":"","name":"Muta","email":"<email>","security_oauth_token":"<bearer>"}
  ```
  `info` = base64(ciphertext).
- `Cosy-Key` = base64(RSA-PKCS#1-encrypt(the 16 key bytes)) under the
  pinned public key:
  ```text
  MIGfMA0GCSqGSIb3DQEBAQUAA4GNADCBiQKBgQDA8iMH5c02LilrsERw9t6Pv5Nc
  4k6Pz1EaDicBMpdpxKduSZu5OANqUq8er4GM95omAGIOPOh+Nx0spthYA2BqGz+l
  6HRkPJ7S236FZz73In/KVuLnwI8JJ2CbuJap8kvheCCZpmAWpb/cPx/3Vr/J6I17
  XcW+ML9FoCI6AOvOzwIDAQAB
  ```
- Authorization payload (fixed field order, signature-sensitive):
  ```json
  {"version":"v1","requestId":"<v4-uuid-hyphenated>","info":"<info>","cosyVersion":"<Cosy-Version>","ideVersion":""}
  ```
- Signature: `MD5(payloadB64 ‖ "\n" ‖ key ‖ "\n" ‖ unixSeconds ‖ "\n" ‖
  encodedBody ‖ "\n" ‖ signedPath)`, lowercase hex, **no trailing LF**.
- Header: `Authorization: Bearer COSY.<payloadB64>.<sigHex>`.

The COSY request UUID is fresh per request (reversed entropy bytes,
RFC-4122 v4 masked); the server rejects duplicates with code `103`.

### 3.6 Header set and conditional presence

Static identity headers (declared in
`request.rs::headers` for the Qoder dialect):

```text
Accept: text/event-stream          Cache-Control: no-cache
Connection: keep-alive             Cosy-ClientType: 5
Cosy-MachineType: 5                Cosy-Version: <pinned>
Cosy-Business-Product: cli         Cosy-Business-Type: agent
Cosy-Scene: assistant              Cosy-Data-Policy: agree|disagree
Login-Version: v2
```

Per-request stamped by the executor: `Authorization`, `Cosy-Date` (unix
seconds), `Cosy-Key`, `Cosy-User` (omitted when empty),
`Cosy-Organization-Id` / `Cosy-Organization-Tags` (omitted when unset).

Presence matrix (from the pinned 1.1.34 WASM characterization):

| Shape | Headers | Omitted |
|---|---|---|
| full (org id + tags + model key) | 22 | none |
| no org, no tags | 20 | `Cosy-Organization-Id`, `Cosy-Organization-Tags` |
| org id, no tags | 21 | `Cosy-Organization-Tags` |
| empty model key | −2 | `X-Model-Key`, `X-Model-Source` |

Known drift: the 1.1.57 auth WASM's data section contains
`Cosy-ClientIp` (absent from the 1.1.34 header table). Trigger conditions
are unverified — re-test against a live token before relying on it.

### 3.7 Response

Plain SSE (no body codec):

```text
data:{"headers":{"Content-Type":["application/json"]},"body":"<chat.completion.chunk JSON>","statusCodeValue":200,"statusCode":"OK"}
```

- `statusCodeValue != 200` is an upstream error (surfaced by
  `qoder::unwrap_envelope` as `Envelope::Error`).
- A `[DONE]` marker inside a 200 envelope is **not** a terminator; the
  authoritative close is the SSE event `event:finish` with duration
  metadata (`{"firstTokenDuration":…,"totalDuration":…,"serverDuration":…}`).
- Error codes seen in the wild: `103` = duplicate `request_id` (never
  auto-retry a COSY request — the UUID must be regenerated), `112` =
  quota/paywall.

### 3.8 Usage / quota

`GET https://openapi.qoder.sh/api/v1/userinfo` with a plain bearer (no
COSY). CN accounts use `openapi.qoder.com.cn`. The response shape is
loose (`data.plan.plan_name`, `data.usage.{remaining,total,used}`);
trial accounts omit usage — the parser degrades to identity-only metrics.
Fetcher: `muta-providers/src/usage/qoder.rs`.

---

## 4. How muta implements it (map)

| Concern | Location |
|---|---|
| Typed request identity | `muta-contracts/src/auth.rs` (`QoderRequestIdentity`) |
| Connection-auth variant | `muta-contracts/src/connection_auth.rs` (`ConnectionAuth::QoderOAuth`) |
| Wire dialect | `muta-contracts/src/catalog.rs` (`OpenAiChatDialect::Qoder`) |
| OAuth preset + `is_qoder()` | `muta-contracts/src/provider_auth.rs` (`qoder_preset`, `DeviceFlow::Qoder`) |
| Body codec, signing, envelope | `muta-llm-client/src/protocol/openai/chat_completions/qoder.rs` |
| Executor routing | `muta-llm-client/src/protocol/openai/chat_completions/mod.rs` (`build_qoder_request`, `send_request` Qoder branch) |
| Device flow + PAT exchange | `muta-providers/src/oauth/qoder.rs` |
| Credential source (OAuth + PAT) | `muta-providers/src/oauth/credential_source.rs`, `oauth/qoder.rs` (`QoderApiKeyCredentialSource`) |
| Durable identity store | `muta-providers/src/oauth/store.rs` (`TokenSet.qoder`, `QoderStoredIdentity`) |
| Login-time identity assembly | `muta-runtime/src/handlers_provider.rs` (`run_oauth` Qoder branch) |
| Provider preset + usage | `muta-providers/src/registry/qoder.rs`, `usage/qoder.rs` |
| TUI template | `apps/terminal/crates/mutx/src/providers.rs` (`id: "qoder"`) |
| Transport derivation | `muta-agent/src/catalog/derive.rs` |

Test suites (all under the packages above, filter `qoder`): 48 tests —
codec round-trips, signature shape, header presence, executor stamping,
device-flow state machine (local TCP mock), PAT exchange, usage parsing.

---

## 5. Maintenance runbook

### 5.1 When Qoder stops working (checklist, in order)

1. **Which layer failed?** A `401` on the OpenAPI surface is auth; a
   `403`/`statusCodeValue: 403` envelope is quota/paywall; a decode error
   in the response path is protocol drift.
2. **Auth first**: verify the token with
   `GET /api/v1/userinfo` (plain bearer, no COSY). If userinfo works but
   inference fails, the signature layer drifted.
3. **Signature drift**: extract the newest client (see §2 recipe), carve
   the auth WASM, and diff its data-section strings against §3. Watch for:
   header-name changes, `Cosy-Version` value bumps, new conditional
   headers, alphabet changes in the QoderEncoding table.
4. **Header presence**: the 20/21/22-header matrix in §3.6 is
   server-sensitive. If the server starts rejecting, re-run the fixture
   matrix (Liki4's `infer-user.json` vector is the reference oracle for
   1.1.34 behavior).
5. **Version alignment**: `Cosy-Version` participates in the signature.
   When bumping the pinned value, it must change in exactly two places
   (the header in `request.rs` and the payload in `prepare_request`) —
   they share the `COSY_VERSION` constant; never fork them.

### 5.2 Known-open items (as of 1.1.57)

- **`Cosy-ClientIp`**: present in the 1.1.57 WASM data section; trigger
  conditions unknown. muta omits it; if the server starts requiring it,
  the shape would be a plain header in `build_qoder_request`.
- **PAT-line `uid`**: the typed identity is minted with an empty uid on
  the ApiKey path (no device-token response to read it from). Whether the
  server accepts an empty `Cosy-User` with a valid PAT is unverified;
  backfilling from `/api/v1/userinfo` is the planned fix if it does not.
- **Risk fingerprinting**: Qoder pins device signals to `machine_id` and
  the persisted AES key. The `state_dir/machine_id` file and
  `TokenSet.qoder.machine_key_hex` are load-bearing for account
  continuity; do not regenerate them per process.
- **Embedded allowlist**: the 1.1.57 WASM carries a 28-uid
  `allowed_user_ids` feature-gate blob; behavior for gated accounts may
  differ.

### 5.3 Extending to the CN line

The CN surface swaps hosts (`*.qoder.com.cn`), the device-flow page
(`qoder.com.cn`), and the client id (`e883ade2-…`). The signing constants
(alphabet, RSA key, header set) are identical in the CN client. muta's
preset currently pins the international line; a CN variant would add a
`qoder-cn` preset selecting the CN hosts and client id — no new signing
code.

### 5.4 Reference artifacts (provenance)

- Recon report: `/tmp/qoder-re/QODER-RECON-REPORT.md`
- Carved 1.1.57 auth WASM: `/tmp/qoder-re/qoder_auth_wasm_1.1.57.wasm`
- Community docs clone: `/tmp/qoder-re/ref-liki4/`
- Community implementations worth consulting when debugging:
  `Liki4/qodercli2api` (Go, byte-exact + fixtures),
  `cubk1/qoder2api` (Java), `fengyinxia/qoder2api` (Python, CN line),
  `EchoPing07/Qoder-2API-Go` (Go + panel).

These live under `/tmp` and will not survive a reboot; the carved WASM and
the client_id constants above are the two artifacts worth re-deriving from
a fresh client download if lost — both recoverable in under an hour with
the §2 recipe.

---

## 6. Design decisions worth remembering

1. **No new `WireProtocol`.** Qoder's chunks are standard chat-completions
   JSON; only the envelope differs. Modeling it as an `OpenAiChatDialect`
   kept the change surgical and reused the existing SSE/echo/reasoning
   plumbing.
2. **Provider-owned identity, no borrowed fields.** The typed
   `QoderRequestIdentity` exists because the first implementation
   overloaded `account_id` (ChatGPT's tenant header) for the uid and
   `project_id` (Google's cloud project) for the machine key. That
   borrowing is exactly how cross-provider debugging confusion starts;
   the typed field keeps each protocol's identity material separate and
   makes the auth store schema self-describing.
3. **PAT over device flow for the common path.** The device flow needs a
   browser round-trip and a 300 s polling window; the PAT path is a paste
   and works as plain ApiKey auth with the COSY identity minted locally.
4. **Signature determinism is not testable the naive way.** The COSY
   request UUID is fresh per call by protocol requirement (code 103), so
   identical inputs must produce different signatures. The tests assert
   key/date stability and signature sensitivity instead of byte-equality.
5. **`machine_id` persistence is a correctness feature, not hygiene.**
   The device-flow correlation and the risk layer both anchor to it; a
   per-process value would effectively re-enroll the device every run.
