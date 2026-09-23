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

Qoder **does** publish a live model list: `GET /algo/api/v2/model/list?Encode=1`
over the same COSY-signed transport as inference, returning a `scene → [entry]`
map. It was mistakenly recorded here as absent in an earlier revision and is now
the authoritative catalog (ADR-0266). The inference protocol below was
reconstructed from the shipped client binaries.

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
| Inference (COSY SSE) | `https://api1/api2/api3.qoder.sh` (**elected**, see §3.1a — currently `api3`) | `https://gateway.qoder.com.cn` |
| Model catalog (COSY) | same hosts, `GET /algo/api/v2/model/list?Encode=1` | same |
| OpenAPI (token, userinfo) | `https://openapi.qoder.sh` | `https://openapi.qoder.com.cn` |
| Device-flow page | `https://qoder.com/device/selectAccounts` | `https://qoder.com.cn/...` |
| Endpoint election | `https://center.qoder.sh/algo/api/v{3,5}/service/region/endpoints` (plain Bearer, QoderEncoding response) | — |
| Daily/test (seen in client) | `daily-api2.qoder.sh`, `test-api2.qoder.sh`, `test-openapi.qoder.sh` | — |

Inference URL (query constants are part of the contract):

```text
POST {base}/algo/api/v2/service/pro/sse/agent_chat_generation
    ?FetchKeys=llm_model_result&AgentId=agent_common&Encode=1
```

The signed path is the URL pathname **without** the `/algo` prefix and
without the query: `/api/v2/service/pro/sse/agent_chat_generation`.

The catalog signed path is `/api/v2/model/list` (same rule). One signer serves
both: the canonical form is identical apart from the path. The catalog request
**must** carry `Cosy-User`; without it the service returns
`403 {"code":"101","message":"Signature invalid"}`, even when the signature
itself is correct. See §5.2.

#### 3.1a Inference endpoint election (verified live 2026-09-21)

`api1/api2/api3.qoder.sh` are **not interchangeable**, and the server tells
the client which one to use. The official CLI syncs at startup:

1. `GET https://center.qoder.sh/algo/api/v5/service/region/endpoints`
   (v3 also serves the same shape) with a **plain Bearer** — no COSY
   signature required.
2. The response body is QoderEncoding-encoded (same codec as §3.4 —
   outer-thirds swap + alphabet remap, then base64). Decoded, it is a
   plain role → endpoint map:
   ```json
   {"inferNodes":[{"url":"https://api3.qoder.sh","type":"public"}],
    "security":[{"url":"https://api2.qoder.sh","type":"public"}],
    "openapiNodes":[{"url":"https://openapi.qoder.sh","type":"public"}],
    "centerNodes":[...], "codebase":[...], "remoteAgent":[...],
    "nesNodes":[...], "fallbackIpMap":{}, "fallbackDomainMap":{}}
   ```
3. The client caches it in `~/.qoder/.cache/endpoint-cache.json` and
   elects `inferNodes[0]` for `agent_chat_generation`.

**Host roles are distinct**: `api2` is the *security* cluster, `api3` (and
`api1`) are the inference nodes. muta originally pinned `api2` — the wrong
cluster — and its per-cluster daily billing counter
(`403 code 110 "Billing daily count exceeded"`, envelope in §3.7) is what
surfaced as the "qodercli unlimited / muta limited" symptom on 2026-09-21:
identical credentials and identical wire bytes passed against `api1`/`api3`
and failed against `api2`, ~10/10 trials each way, while `qodercli` (elected
`api3`) kept working throughout. muta now pins `api3`, the server-issued
inference endpoint, in
`muta_providers::registry::qoder::MODEL_PROVIDER_SPEC`.

A fuller election implementation (sync-on-login, cache the elected endpoint
in the connection's auth attributes, fall back to the pinned constant) is
deliberately deferred: the region map currently hard-assigns `api3` and the
recovery recipe below is cheaper than the added surface. If inference
starts failing cluster-wide, re-run the sync above and diff `inferNodes`
against the pinned constant before suspecting the wire.

**Implemented (ADR-0272, 2026-09-21).** The election now runs as part of
muta's credential resolution: the center map is synced once per connection
(OAuth credentials ride the uid-repair critical section; PAT credentials
ride the identity mint), the decoded `inferNodes[0].url` is adopted only
after the strict https `*.qoder.sh` allowlist check, and it is persisted in
the stored identity (`QoderStoredIdentity.infer_endpoint`,
`TokenSet.attributes["qoder"]`). The inference signer derives its URL from
the elected endpoint (`RequestSignerPhase::request_url` now carries the
resolved auth for exactly this purpose), the catalog fetch root follows the
same host, and any sync failure leaves the pin
(`MODEL_PROVIDER_SPEC.root_url`, currently `api3`) authoritative —
failure never diminishes a connection (ADR-0227). The sync recipe above is
now automated; re-running it manually remains the diagnosis step when both
the election and the pin fail together.

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

### 3.3 Request identity (typed, provider-owned, ADR-0267)

Qoder's identity material lives in `muta_providers::registry::qoder::QoderRequestIdentity`,
attached to `ResolvedAuth` via `ExtensionMap` (`auth.extension::<QoderRequestIdentity>()`)
— deliberately **not** reusing ChatGPT `account_id` or Google `project_id`, and completely
decoupled from the core contracts crate. One instance per connection, persisted in the auth
store under `TokenSet.attributes["qoder"]` as `QoderStoredIdentity`.

| Field | Feeds | Persistence rule |
|---|---|---|
| `uid` | `Cosy-User` header + payload `uid` | from the device-token/exchange response |
| `machine_key_hex` | AES-128 key/IV (16 bytes) + `Cosy-Key` (RSA-wrapped) | generated once per device, never rotated (rotating reads as device churn to the risk layer) |
| `data_policy_agreed` | `Cosy-Data-Policy` (`agree`/`disagree`) | set at login |
| `organization_id` | `Cosy-Organization-Id` (omitted when empty) | from userinfo, when the account has one |
| `organization_tags` | `Cosy-Organization-Tags` (comma-joined, order preserved) | from userinfo |
| `infer_endpoint` | inference/catalog transport host (§3.1a election) | synced once from the center region map; `None` (serde default) keeps the pinned spec root authoritative |

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

Implementation: `muta-providers/src/registry/qoder/wire/codec.rs`
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
  auto-retry a COSY request — the UUID must be regenerated), `110` =
  per-cluster daily billing counter exceeded (see §3.1a — cluster
  selection matters; it is **not** an account-wide paywall), `112` =
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
| Typed request identity | `muta-providers/src/registry/qoder/identity.rs` (`QoderRequestIdentity`) |
| Connection-auth variant | `muta-contracts/src/connection_auth.rs` (`ConnectionAuth::Subscription { provider: "qoder" }`) |
| Phased wire pipeline | `muta-providers/src/registry/qoder/pipeline.rs` (`build_qoder_pipeline`) |
| OAuth preset + Flow | `muta-providers/src/oauth/presets.rs` (`qoder_preset`, `DeviceFlowMode::custom("qoder")`) |
| Body codec, signing, envelope | `muta-providers/src/registry/qoder/wire/` (`codec.rs`, `signer.rs`, `envelope.rs`) |
| Stream frame transformer | `muta-providers/src/registry/qoder/wire/stream.rs` (`QoderStreamTransformer`) |
| Executor routing | Generic `muta-llm-client` via `TransportPipeline` (zero vendor branches) |
| Device flow + PAT exchange | `muta-providers/src/oauth/qoder.rs` |
| Credential source (OAuth + PAT) | `muta-providers/src/oauth/credential_source.rs`, `oauth/qoder.rs` (`QoderApiKeyCredentialSource`) |
| Durable identity store | `muta-providers/src/oauth/store.rs` (`TokenSet.attributes["qoder"]`) |
| Login-time identity assembly | `muta-providers/src/oauth/enricher.rs` (`QoderOAuthEnricher`) |
| Provider preset + usage | `muta-providers/src/registry/qoder/`, `usage/qoder.rs` |
| TUI template | `apps/terminal/crates/mutx/src/providers.rs` (`id: "qoder"`) |
| Transport derivation | `muta_providers::build_credential_source` and `build_qoder_pipeline` |

Test suites (all under the packages above, filter `qoder`): 48 tests —
codec round-trips, signature shape, header presence, executor stamping,
device-flow state machine (local TCP mock), PAT exchange, usage parsing.

---

## 5. Maintenance runbook

### 5.1 When Qoder stops working (checklist, in order)

1. **Which layer failed?** A `401` on the OpenAPI surface is auth; a
   `403`/`statusCodeValue: 403` envelope is quota/paywall; a decode error
   in the response path is protocol drift.
2. **Which cluster failed?** `code 110` ("Billing daily count exceeded")
   is a **per-cluster daily counter**. Reproduce the identical request
   against `api3` and `api1` before touching anything else (the
   `qoder_live_smoke` example takes a one-line base-URL edit). If another
   inference node accepts it, the elected endpoint drifted — re-sync
   `center.qoder.sh/algo/api/v5/service/region/endpoints` (§3.1a) and
   repoint the pin.
3. **Auth first**: verify the token with
   `GET /api/v1/userinfo` (plain bearer, no COSY). If userinfo works but
   inference fails, the signature layer drifted.
4. **Signature drift**: extract the newest client (see §2 recipe), carve
   the auth WASM, and diff its data-section strings against §3. Watch for:
   header-name changes, `Cosy-Version` value bumps, new conditional
   headers, alphabet changes in the QoderEncoding table.
5. **Header presence**: the 20/21/22-header matrix in §3.6 is
   server-sensitive. If the server starts rejecting, re-run the fixture
   matrix (Liki4's `infer-user.json` vector is the reference oracle for
   1.1.34 behavior).
6. **Version alignment**: `Cosy-Version` participates in the signature.
   Its value has exactly one home: `IdentitySpec.emulated_version` on the
   surface (`muta_providers::registry::qoder::surface`). The
   `version_header` (`Cosy-Version`), the signature payload's `cosyVersion`,
   and the envelope's `business.version` all read it from there. Never fork
   it (ADR-0265).

### 5.2 Root cause of the live 403 (found and fixed 2026-09-20)

**The 16-byte AES key must be 16 ASCII characters, not 16 binary bytes.**

The server UTF-8-decodes the RSA-unwrapped `Cosy-Key` and uses the result as the
AES key/IV for `info`. Mutas hand-rolled `encrypt_info` decoded the persisted
32-character `machine_key_hex` into 16 **binary** bytes; those bytes are
normally not valid UTF-8, and the server rejects the whole request with
`403 {"code":"101","message":"Signature invalid"}`.

Established by replaying hand-built pairs against the live 1.1.58 endpoint,
with the request shape held constant and only the key bytes varied:

| AES key bytes | Result |
|---|---|
| 16 ASCII characters (e.g. `b"a"*16`, `"0123456789abcdef"`) | **200** |
| 16 bytes of a UTF-8-valid multi-byte string | **200** |
| 16 binary bytes (`bytes(range(16))`, the decoded stored key, random) | **403** |
| 16 bytes that are invalid UTF-8 (`0xc8..`, `0xff*16`) | **403** |

Reproducible 12/12 trials each way, interleaved, with a known-good control
passing throughout (so this is content-dependence, not rate limiting).

The real client's own derivation, recovered by executing its 1.1.58 auth WASM
(`generate_runtime_auth_fields`) under the reference oracle, is
`runtimeASCIIKey(uuid) = hex(uuid[..8])` — i.e. **the hex string itself**, 16
ASCII characters. Mutas persisted key is `hex(b0..b15)`; its first 16 characters
are `hex(b0..b7)`, byte-identical to the real client's shape. So the fix reads
the first 16 characters as ASCII, and **existing credentials keep working with
no re-authorization and no device-identity churn**.

Verified after the fix: mutas own catalog path returns
`["qfmodel", "qmodel_38max"]` live, and its inference path streams a real
completion.

### 5.2a Second, separate bug: the SSE stream is nested

Inference now authenticates (200) and the service streams, but the events are
wrapped:

```text
data:{"headers":{"Content-Type":["application/json"]},"body":"{\"choices\":[{\"delta\":{...}}]}","statusCodeValue":200,"statusCode":"OK"}
```

The chat-completions object is a **JSON string inside `body`**, not the top-level
`data:` payload. Mutas parser reads `data["choices"]` directly, so it yields no
deltas (an empty completion). This is a decoding gap, not an auth failure, and is
not yet fixed.

### 5.3 Known-open items (as of 1.1.58)

- **SSE envelope nesting** (§5.2a) — the remaining blocker for usable inference
  output.
- **`Cosy-ClientIp`**: present in the client's WASM data section; trigger
  conditions unknown. muta omits it; live tests show the catalog accepts its
  absence. If the server starts requiring it, the shape is a plain header in
  the surface's identity table.
- **Endpoint election** (§3.1a): implemented per ADR-0272 — the center
  region map is synced once per credential, the elected `inferNodes[0].url`
  is allowlisted (`https://*.qoder.sh`) and persisted in the stored
  identity, and both the inference signer and the catalog root consume it
  with the pin (`api3`) as fallback. If Qoder's region map re-points
  `inferNodes`, no code change is needed; the pin and the
  `qoder_live_smoke` example (same constant, one line) only matter when
  the election has not synced yet.
- **Risk fingerprinting**: Qoder pins device signals to `machine_id` and
  the persisted AES key. The `state_dir/machine_id` file and
  `TokenSet.qoder.machine_key_hex` are load-bearing for account
  continuity; do not regenerate them per process.
- **Embedded allowlist**: the 1.1.58 WASM carries a 28-uid
  `allowed_user_ids` feature-gate blob; behavior for gated accounts may
  differ.

### 5.2a Verified live with 1.1.58 (2026-09-20)

Confirmed against the live service by replaying byte-exact requests:

- The `agent_chat_generation` **envelope is mandatory**: a flat
  chat-completions body returns `400 {"code":…,"message":"None flow nodes
  found for router agent_router"}`; the same request wrapped in the envelope
  streams `200`. See ADR-0265.
- The **catalog signature is bound to `Cosy-User`**: with it, `200`; without
  it (or with an empty value), `403 code 101`. See §5.2.
- The catalog **response is a plain JSON `scene → [entry]` map** (not
  encrypted, despite `Encode=1`); `assistant` exposes 25 keys of which only
  `qmodel_38max` (Qwen3.8-Max) and `qfmodel` (Qwen3.8-Flash) are
  `enable:true`. The `experts` scene omits `qfmodel` entirely.
- `Cosy-Version` (now `1.1.58`) and the signed-path derivation are unchanged
  from 1.1.57; the signature algorithm is confirmed by reproducing the
  recorded request's signature exactly.
- The **full server catalog shape** (captured live with
  `cargo run -p muta-providers --example qoder_catalog_dump`): eleven scenes
  (`assistant`, `chat`, `quest`, `qwork`, `experts`, `qwake`, `app`, `nap`,
  `inline`) plus empty `byok_teams`/`byok_enterprise` maps; each populated
  scene carries the routing switches alongside real models (`smodel` Sonus,
  `cmodel` Cantus, `qmodel_38max`, `qfmodel`, `qmodel_latest`, `qmodel`,
  `kmodel_latest`, `kmodel`, `gmodel`, `gfmodel`, `dmodel`, `dfmodel`,
  `mmodel`). For a free account only the two flagships are `enable:true`;
  everything else is subscription-locked. The official CLI's `--list-models`
  prints exactly the two enabled entries, while its interactive `/model` menu
  lists the locked entries greyed-out (`isGreyedOut: enable === false`). muta
  mirrors both behaviors: `--list-models` parity is the inference-capable set,
  and the picker surfaces locked entries dimmed, stating the provider's own
  reason when the payload carries one.

#### Function switches vs models (ADR-0281)

A scene mixes two **kinds** of entry, and they are not interchangeable:

| Kind | Keys | What selecting one means |
|------|------|--------------------------|
| Model | `qmodel_38max`, `qfmodel`, `smodel`, `gmodel`, … | Run *this* model |
| Function switch | `auto`, `ultimate`, `performance`, `efficient`, `advanced` | *Let the server pick* a model |

The switches are not models: they carry no capability, and `performance` even
advertises a `272K` context window that no real entry has. Sending one as
`X-Model-Key` delegates model choice upstream, so every capability muta fitted
for the channel (effort ladder, thinking, context window) describes a model
nobody selected. muta therefore **excludes them at parse time** — a membership
fact, not an availability verdict — via
`surface::is_function_switch(key, scene)`. Scene-scoped forms carry their
owning scene as a hyphen prefix (`quest-auto`, `qwork-advanced`,
`experts-ultimate`) and are matched in that scene; model keys use `_`, so the
namespaces cannot collide.

The payload declares **no kind field** — no value of any field is disjoint
between the two kinds except `display_name` and `key` themselves. `is_new` is
present on every captured model and absent from every switch, but it is a
"NEW" marketing badge, so it is a drift *audit* signal and never the
classifier. The vocabulary is pinned in `surface::FUNCTION_SWITCH_KEYS` and
audited against the committed capture
(`tests/it/qoder_catalog_contract.rs`); a new switch name fails that audit
rather than silently entering the model list.

#### The locked reason

A locked entry carries `strategies[] = [{ tag, priority, enabled,
disabled_message_key }]`. `disabled_message_key` is an opaque i18n key (e.g.
`codeSafeModelReason`, resolved by the client's own `dynamic-texts.json`), and
muta records it **verbatim** as `Availability.reason` — it is never resolved
against the vendor's table, translated, or branched on. An
entry the payload gives no reason for keeps `reason: None`; undeclared is never
invented.

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
2. **Provider-owned identity, no borrowed fields (ADR-0267).** The typed
   `QoderRequestIdentity` lives inside `muta-providers` and mounts onto
   `ResolvedAuth` via `ExtensionMap`, persisted under `TokenSet.attributes["qoder"]`.
   It completely avoids polluting core contracts while keeping identity
   material separate and making the auth store schema self-describing.
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
