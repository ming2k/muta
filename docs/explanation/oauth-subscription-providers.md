# OAuth2 Subscription Providers and Internal Protocols

Modern AI development environments frequently integrate subscription accounts
(such as Google One AI Premium, ChatGPT Plus/Team/Pro, and GitHub Copilot) in
addition to standard pay-as-you-go API keys.

Subscription providers do not issue static API keys. Instead, they authenticate
via OAuth2 flows, require dedicated client fingerprinting, negotiate cloud
projects, and enforce dynamic tiered rate limits. This page documents the
architecture and lifecycle used by muta to manage subscription connections.

For general provider configuration, see [Providers](../reference/providers.md).
For capability derivation details, see
[Provider Multi-Strategy Adaptation Architecture](provider-strategy-architecture.md).

```text
┌──────────────┐         ┌────────────────────────┐         ┌───────────────────────┐
│ Browser Auth │ ──────> │ Local Loopback Server  │ ──────> │ Atomic AuthStore      │
│ (PKCE S256)  │         │ (Port 8085 / Callback) │         │ (Durable Credentials) │
└──────────────┘         └────────────────────────┘         └───────────┬───────────┘
                                                                        │
                                 ┌──────────────────────────────────────┘
                                 │
                     ┌───────────▼────────────┐
                     │ Background Refresher   │
                     │ (T - 120s Pre-Expiry)  │
                     └───────────┬────────────┘
                                 │
                     ┌───────────▼────────────┐
                     │ Provider Driver Layer  │
                     │ (Fingerprints & Quota) │
                     └────────────────────────┘
```

---

## 1. OAuth2 PKCE Authentication Lifecycle

All subscription connections in muta implement OAuth 2.0 with Proof Key for
Code Exchange (PKCE, RFC 7636) using the SHA-256 code challenge method.

### The Loopback Authorization Flow

1. **Challenge Generation**: The client generates a high-entropy cryptographically
   random `code_verifier`, derives the `code_challenge = BASE64URL(SHA256(verifier))`,
   and generates a random `state` and `nonce`.
2. **Local Callback Listener**: A transient local TCP listener binds to a loopback
   address (e.g. `http://localhost:8085/oauth/callback`).
3. **Browser Delegation**: The default system browser opens the provider's
   authorization URL.
4. **Exchange and Validation**: Upon user consent, the provider redirects to the
   loopback listener with an authorization code. The client validates that the
   received `state` matches the session state before issuing a `POST` request
   to the provider token endpoint with the `code_verifier`.

### Token Storage and Proactive Renewal

Tokens are stored on disk in the application state directory using atomic file
writes protected by a process-level file lock (`AuthStore`).

Access tokens expire after a bounded lifetime (typically 1 hour). To prevent
in-flight request failures during long reasoning rounds, muta inspects token
expiration timestamps and initiates a refresh when the token is within
`ACCESS_TOKEN_REFRESH_SKEW_MS` (2 minutes) of expiry.

---

## 2. Google Antigravity CodeAssist Protocol

Google Antigravity routes requests through internal Google Cloud Code Assist
endpoints (`https://cloudcode-pa.googleapis.com` and
`https://daily-cloudcode-pa.googleapis.com`).

```text
┌───────────────────────────────┐
│ Google OAuth Token Acquired   │
└───────────────┬───────────────┘
                │
┌───────────────▼───────────────┐
│ POST /v1internal:             │
│ loadCodeAssist                │
└───────────────┬───────────────┘
                │ Project missing?
       ┌────────┴────────┐
       │ (Yes)           │ (No)
┌──────▼────────┐        │
│ POST /v1inter:│        │
│ onboardUser   │        │
└──────┬────────┘        │
       └────────┬────────┘
                │
┌───────────────▼───────────────┐
│ Resolve cloudaicompanion-     │
│ Project Context               │
└───────────────┬───────────────┘
                │
       ┌────────┴────────────────────────┐
       │                                 │
┌──────▼────────────────────────┐ ┌──────▼────────────────────────┐
│ POST /v1internal:             │ │ POST /v1internal:             │
│ fetchAvailableModels          │ │ retrieveUserQuotaSummary      │
└───────────────────────────────┘ └───────────────────────────────┘
```

### Client Fingerprint and Request Headers

The CodeAssist gateway requires specific client headers matching official
internal toolchains:

- `User-Agent: Antigravity/1.23.2 (Linux; x86_64)`
- `x-goog-api-client: gl-go/1.23.2 gdcl/0.1`

Requests lacking these headers receive `403 Forbidden` or generic gateway errors.

### Project Onboarding and Discovery

Calls to Antigravity services must specify a tenant Google Cloud project:

1. **`loadCodeAssist`**: Issues a metadata payload (`ideType: "ANTIGRAVITY"`,
   `pluginType: "GEMINI"`). If the account has an initialized workspace, the
   response returns the active `cloudaicompanionProject`.
2. **`onboardUser`**: If no project exists, muta submits an onboarding request
   with the user's detected subscription tier (`g1-pro-tier` or `paidTier`)
   to provision the backing project.

### Quota Summary and Model Buckets

Quota status is retrieved from `POST /v1internal:retrieveUserQuotaSummary`.
The response contains structured `QuotaSummaryBucket` records:

- `bucket_id`: Model or capability identifier (e.g. `gemini-3.7-flash`,
  `gemini-3.1-pro`).
- `remaining_fraction`: Floating-point scalar from `0.0` (exhausted) to `1.0`
  (fully available).
- `window`: Quota reset window (such as `"DAY"` or `"MINUTE"`).
- `reset_time`: Protobuf timestamp indicating when the quota replenishes.

### Tiered Routing and 429 Backoff

The Antigravity service exposes `gemini-3.7-flash-tiered` for dynamic traffic
routing. When primary capacity is constrained, requests adapt to available
tiers. If a model's `remaining_fraction` reaches `0.0` or returns HTTP `429`
with a `QuotaFailure` payload, muta reads the replenishment timestamp to
prevent redundant retries.

---

## 3. Other Subscription Protocols

| Provider | Authentication Type | Protocol Specialty | Account Scoping |
|----------|---------------------|--------------------|-----------------|
| **Antigravity** | Google OAuth2 PKCE | Internal `v1internal` Protobuf JSON gateway | Google Cloud Project (`cloudaicompanionProject`) |
| **ChatGPT Codex** | OpenAI OAuth2 PKCE | Internal Responses gateway | `ChatGPT-Account-Id` tenant header |
| **GitHub Copilot** | GitHub OAuth App | Copilot session token exchange endpoint | User token exchange |

### ChatGPT Codex Gateway

ChatGPT subscription connections use OAuth tokens to access internal
endpoint surfaces (`/backend-api/codex/models` and `/backend-api/codex/completions`).
The client passes a session-derived `ChatGPT-Account-Id` header to scope
inference to the user's personal or team workspace.

### GitHub Copilot Token Exchange

GitHub Copilot connections authenticate using a personal GitHub OAuth token,
which is exchanged periodically for a short-lived Copilot API bearer token
via `GET https://api.github.com/copilot_internal/v2/token`.
