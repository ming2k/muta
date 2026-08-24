<script lang="ts">
  import { onMount } from "svelte";
  import { daemon } from "../stores/daemon.svelte.js";
  import type { WebSearchConfigUpdate, WebSearchConfigView } from "../types.js";

  interface Props {
    onclose: () => void;
  }

  let { onclose }: Props = $props();

  // Fetch the authoritative snapshot when the dialog opens.
  onMount(() => {
    daemon.queryWebSearchConfig();
  });

  const BACKENDS = [
    { id: "exa", label: "Exa", desc: "hosted MCP · anonymous by default (default)" },
    { id: "parallel", label: "Parallel", desc: "hosted MCP · anonymous by default" },
    { id: "duckduckgo", label: "DuckDuckGo", desc: "keyless scraping · frequently blocked" },
    { id: "searxng", label: "SearXNG", desc: "self-hosted · keyless · needs a URL" },
    { id: "tavily", label: "Tavily", desc: "hosted · needs a Tavily key" },
    { id: "bocha", label: "Bocha", desc: "hosted AI search · needs a key · China-direct" },
  ] as const;

  const READERS = [
    { id: "builtin", label: "Built-in", desc: "direct fetch + local HTML stripping (no JS)" },
    { id: "jina", label: "Jina Reader", desc: "r.jina.ai · JS rendering + readability extraction" },
  ] as const;

  let cfg = $derived(daemon.websearchConfig);

  // Local drafts for text inputs; submitted as PATCH fields on Save.
  let searxngUrl = $state<string | null>(null);
  let keyDrafts = $state<Record<string, string>>({});

  function effectiveSearxngUrl(): string {
    return searxngUrl ?? cfg?.searxng_url ?? "";
  }
  function keyDraft(id: string): string {
    return keyDrafts[id] ?? "";
  }

  function setKeyDraft(id: string, value: string) {
    keyDrafts[id] = value;
  }

  let searxngRequired = $derived(cfg?.provider === "searxng" || cfg?.fallback === "searxng");

  let searxngInvalid = $derived(
    searxngRequired && effectiveSearxngUrl().trim() === "",
  );

  let saveDisabled = $derived(searxngInvalid);

  function save() {
    if (!cfg || saveDisabled) return;
    const patch: Record<string, string> = {};
    if (searxngUrl !== null) {
      patch.searxng_url = searxngUrl.trim();
    }
    for (const [id, value] of Object.entries(keyDrafts)) {
      // Only submit non-empty drafts; clearing a stored key is the explicit
      // ✕ button next to each field, not an empty submit.
      if (value.trim() !== "") patch[id] = value.trim();
    }
    daemon.updateWebSearchConfig(patch as Partial<WebSearchConfigUpdate>);
    // Keep the dialog open: the ack toast confirms, and the presence flags
    // re-render from the authoritative snapshot.
    searxngUrl = null;
    keyDrafts = {};
  }

  function handleBackdrop(e: MouseEvent) {
    if (e.target === e.currentTarget) onclose();
  }

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === "Escape") onclose();
  }
</script>

<svelte:window onkeydown={handleKeydown} />

<div class="backdrop" onclick={handleBackdrop} role="presentation">
  <div class="modal" role="dialog" aria-label="Web search settings">
    <div class="modal-header">
      <h3>Web search</h3>
      <button class="close" aria-label="Close" onclick={onclose}>×</button>
    </div>

    <div class="modal-body">
      {#if !cfg}
        <div class="loading">Loading configuration…</div>
      {:else}
        <section>
          <h4>Search backend <span class="tag">websearch</span></h4>
          <p class="section-hint">
            Used by the <code>websearch</code> tool. Changes apply live and persist to
            <code>config.toml</code>.
          </p>
          <div class="option-grid">
            {#each BACKENDS as b (b.id)}
              <button
                class="option"
                class:active={cfg.provider === b.id}
                onclick={() => daemon.updateWebSearchConfig({ provider: b.id })}
                title={b.desc}
              >
                <span class="option-label">{b.label}</span>
                <span class="option-desc">{b.desc}</span>
              </button>
            {/each}
          </div>
        </section>

        <section>
          <h4>Fallback backend</h4>
          <p class="section-hint">
            Tried automatically when the primary fails. “None” disables the fallback.
          </p>
          <div class="option-grid">
            <button
              class="option"
              class:active={cfg.fallback.trim() === ""}
              onclick={() => daemon.updateWebSearchConfig({ fallback: "" })}
              title="disable the fallback"
            >
              <span class="option-label">None</span>
              <span class="option-desc">no automatic failover</span>
            </button>
            {#each BACKENDS as b (b.id)}
              <button
                class="option"
                class:active={cfg.fallback === b.id}
                onclick={() => daemon.updateWebSearchConfig({ fallback: b.id })}
                title={b.desc}
              >
                <span class="option-label">{b.label}</span>
              </button>
            {/each}
          </div>
        </section>

        <section>
          <h4>Page reader <span class="tag">webfetch</span></h4>
          <p class="section-hint">
            How <code>webfetch</code> converts HTML pages to text. Jina renders JavaScript and
            extracts the main content; the built-in reader is zero-dependency but naive.
          </p>
          <div class="option-grid">
            {#each READERS as r (r.id)}
              <button
                class="option"
                class:active={cfg.reader === r.id}
                onclick={() => daemon.updateWebSearchConfig({ reader: r.id })}
                title={r.desc}
              >
                <span class="option-label">{r.label}</span>
                <span class="option-desc">{r.desc}</span>
              </button>
            {/each}
          </div>
        </section>

        <section>
          <h4>Timeout</h4>
          <div class="timeout-row">
            <button
              class="btn-secondary"
              onclick={() => daemon.updateWebSearchConfig({ timeout_secs: Math.max(5, cfg.timeout_secs - 5) })}
            >
              −5s
            </button>
            <span class="timeout-value">{cfg.timeout_secs} s</span>
            <button
              class="btn-secondary"
              onclick={() => daemon.updateWebSearchConfig({ timeout_secs: cfg.timeout_secs + 5 })}
            >
              +5s
            </button>
          </div>
        </section>

        <section>
          <h4>SearXNG endpoint</h4>
          <label class="field">
            <span class="label">JSON search URL</span>
            <input
              type="text"
              value={effectiveSearxngUrl()}
              oninput={(e) => (searxngUrl = e.currentTarget.value)}
              placeholder="http://localhost:8080/search"
              spellcheck="false"
              class:invalid={searxngInvalid}
            />
            <span class="hint">
              {#if searxngRequired}
                Required — a backend is set to <code>searxng</code>.
              {:else}
                Only used when a backend is <code>searxng</code>.
              {/if}
            </span>
          </label>
        </section>

        <section>
          <h4>API keys</h4>
          <p class="section-hint">
            Persist to <code>credentials.toml</code> (never <code>config.toml</code>). Existing
            keys are never echoed back — only whether they are set. Submit an empty field as a
            no-op; use the ✕ button to clear a stored key.
          </p>
          {#each [
            { id: "exa_api_key", label: "Exa", set: cfg.exa_api_key_set, req: false },
            { id: "parallel_api_key", label: "Parallel", set: cfg.parallel_api_key_set, req: false },
            { id: "tavily_api_key", label: "Tavily", set: cfg.tavily_api_key_set, req: true },
            { id: "bocha_api_key", label: "Bocha", set: cfg.bocha_api_key_set, req: true },
            { id: "jina_api_key", label: "Jina Reader", set: cfg.jina_api_key_set, req: false },
          ] as k (k.id)}
            <label class="field key-field">
              <span class="label">
                {k.label}
                {#if k.set}<span class="key-set">set</span>{:else if k.req}<span class="key-missing">required when selected</span>{/if}
              </span>
              <span class="key-row">
                <input
                  type="password"
                  value={keyDraft(k.id)}
                  oninput={(e) => setKeyDraft(k.id, e.currentTarget.value)}
                  placeholder={k.set ? "(unchanged)" : "not set"}
                  spellcheck="false"
                  autocomplete="off"
                />
                {#if k.set}
                  <button
                    class="btn-secondary"
                    title="Clear the stored key"
                    onclick={() =>
                      daemon.updateWebSearchConfig({ [k.id]: "" } as Partial<WebSearchConfigUpdate>)}
                  >
                    ✕
                  </button>
                {/if}
              </span>
            </label>
          {/each}
        </section>
      {/if}
    </div>

    <div class="modal-footer">
      <span class="footer-note">
        Backend/reader/fallback/timeout apply immediately; text fields need Save.
      </span>
      <button class="btn-secondary" onclick={onclose}>Close</button>
      <button class="btn-primary" disabled={saveDisabled} onclick={save}>Save</button>
    </div>
  </div>
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.55);
    display: flex;
    align-items: flex-start;
    justify-content: center;
    padding-top: 8vh;
    z-index: 100;
  }

  .modal {
    width: 560px;
    max-width: calc(100vw - 32px);
    max-height: 84vh;
    overflow-y: auto;
    background-color: var(--bg-sidebar);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-lg);
    box-shadow: 0 12px 40px rgba(0, 0, 0, 0.5);
    display: flex;
    flex-direction: column;
  }

  .modal-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: 14px 16px;
    border-bottom: 1px solid var(--border-subtle);
  }

  .modal-header h3 {
    font-size: 14px;
    font-weight: 600;
    color: var(--text-primary);
  }

  .close {
    background: transparent;
    border: none;
    color: var(--text-muted);
    font-size: 18px;
    cursor: pointer;
    line-height: 1;
  }

  .close:hover {
    color: var(--text-primary);
  }

  .modal-body {
    padding: 16px;
    display: flex;
    flex-direction: column;
    gap: 20px;
  }

  .loading {
    color: var(--text-muted);
    font-size: 13px;
    padding: 24px 0;
    text-align: center;
  }

  section h4 {
    font-size: 12px;
    font-weight: 600;
    color: var(--text-primary);
    margin: 0 0 4px;
  }

  .tag {
    font-family: var(--font-mono);
    font-size: 9px;
    color: var(--text-muted);
    border: 1px solid var(--border-subtle);
    border-radius: var(--radius-sm);
    padding: 1px 5px;
    margin-left: 6px;
    vertical-align: middle;
    text-transform: lowercase;
  }

  .section-hint {
    font-size: 11px;
    color: var(--text-muted);
    margin: 0 0 10px;
    line-height: 1.5;
  }

  .section-hint code,
  .hint code {
    font-family: var(--font-mono);
    background: rgba(255, 255, 255, 0.06);
    padding: 1px 4px;
    border-radius: var(--radius-sm);
  }

  .option-grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(160px, 1fr));
    gap: 8px;
  }

  .option {
    display: flex;
    flex-direction: column;
    gap: 3px;
    text-align: left;
    background: var(--bg-surface);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-md);
    padding: 9px 11px;
    cursor: pointer;
    color: var(--text-secondary);
  }

  .option:hover {
    border-color: var(--border-input-focus);
  }

  .option.active {
    border-color: var(--accent-primary);
    background: rgba(255, 255, 255, 0.04);
  }

  .option.active .option-label {
    color: var(--accent-primary);
  }

  .option-label {
    font-size: 12px;
    font-weight: 600;
    color: var(--text-primary);
  }

  .option-desc {
    font-size: 10px;
    color: var(--text-muted);
    line-height: 1.4;
  }

  .timeout-row {
    display: flex;
    align-items: center;
    gap: 12px;
  }

  .timeout-value {
    font-family: var(--font-mono);
    font-size: 14px;
    color: var(--text-primary);
    min-width: 48px;
    text-align: center;
  }

  .field {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }

  .label {
    font-family: var(--font-mono);
    font-size: 10px;
    color: var(--text-muted);
    text-transform: uppercase;
    letter-spacing: 0.5px;
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .key-set {
    color: var(--accent-success, #3fb950);
    border: 1px solid currentColor;
    border-radius: var(--radius-sm);
    padding: 0 4px;
    text-transform: none;
  }

  .key-missing {
    color: var(--accent-warning);
    text-transform: none;
  }

  input {
    background-color: var(--input-bg-inactive);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-md);
    padding: 8px 10px;
    color: var(--text-primary);
    font-size: 13px;
    font-family: var(--font-mono);
    outline: none;
    width: 100%;
    box-sizing: border-box;
  }

  input:focus {
    border-color: var(--border-input-focus);
    background-color: var(--input-bg-active);
  }

  input.invalid {
    border-color: var(--accent-danger);
  }

  .key-field {
    margin-bottom: 10px;
  }

  .key-row {
    display: flex;
    gap: 6px;
    align-items: stretch;
  }

  .hint {
    font-size: 11px;
    color: var(--text-muted);
    line-height: 1.5;
  }

  .modal-footer {
    padding: 12px 16px;
    border-top: 1px solid var(--border-subtle);
    display: flex;
    justify-content: flex-end;
    align-items: center;
    gap: 8px;
  }

  .footer-note {
    font-size: 10px;
    color: var(--text-muted);
    margin-right: auto;
  }

  .btn-primary,
  .btn-secondary {
    font-size: 12px;
    font-weight: 500;
    padding: 7px 14px;
    border-radius: var(--radius-md);
    cursor: pointer;
    border: 1px solid transparent;
  }

  .btn-primary {
    background-color: var(--accent-primary);
    color: #fff;
    border: none;
  }

  .btn-primary:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }

  .btn-secondary {
    background: transparent;
    border-color: var(--border-strong);
    color: var(--text-secondary);
  }

  .btn-secondary:hover {
    background: var(--bg-surface);
  }
</style>
