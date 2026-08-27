<script lang="ts">
  import { daemon } from "../stores/daemon.svelte.js";
  import { roundActiveMs, roundTps } from "../types.js";

  interface Props {
    onToggleSidebar: () => void;
    onOpenModels: () => void;
    onOpenWebSearch: () => void;
  }

  let { onToggleSidebar, onOpenModels, onOpenWebSearch }: Props = $props();

  function formatDuration(ms: number): string {
    if (ms < 1000) return `${ms}ms`;
    return `${(ms / 1000).toFixed(1)}s`;
  }

  let roundLabel = $derived.by(() => {
    const r = daemon.lastRound;
    if (!r) return null;
    const tps = roundTps(r);
    const parts = [
      `${r.output_tokens.toLocaleString()} tok`,
      formatDuration(roundActiveMs(r)),
    ];
    if (tps > 0) parts.push(`${tps.toFixed(1)} tok/s`);
    return parts.join(" · ");
  });
</script>

<header class="header">
  <button class="icon-btn menu-btn" aria-label="Toggle sessions" onclick={onToggleSidebar}>
    <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
      <path d="M3 6h18M3 12h18M3 18h18"/>
    </svg>
  </button>

  <div class="session-info">
    <h2 class="title">
      {daemon.activeSession?.overview || daemon.activeSessionId || "No active session"}
    </h2>
    <div class="meta">
      {#if daemon.activeSession}
        {#if daemon.roundCounter > 0}
          <span class="tag">round {daemon.roundCounter}</span>
        {/if}
        {#if daemon.currentTurn !== null && daemon.isBusy}
          <span class="tag">turn {daemon.currentTurn + 1}</span>
        {/if}
        {#if daemon.contextTokens}
          <span class="tag">{daemon.contextTokens.toLocaleString()} ctx</span>
        {/if}
        {#if daemon.activity && daemon.isBusy}
          <span class="tag activity">{daemon.activity}</span>
        {:else if daemon.activeSession.current_tool}
          <span class="tag">{daemon.activeSession.current_tool}</span>
        {/if}
        {#if roundLabel && !daemon.isBusy}
          <span class="tag">{roundLabel}</span>
        {/if}
      {/if}
    </div>
  </div>

  <div class="actions">
    {#if daemon.delegated}
      <span class="badge delegated" title="Delegated: permission prompts are bypassed">
        delegated
      </span>
    {/if}
    {#if daemon.providerInfo}
      <button class="model-btn" title="Switch model" onclick={onOpenModels}>
        <span class="provider">{daemon.providerInfo.provider}</span>
        <span class="model">{daemon.providerInfo.model}</span>
      </button>
    {/if}
    <button
      class="model-btn"
      title="Web search backend & reader settings"
      onclick={onOpenWebSearch}
    >
      <span class="provider">⌕</span>
      <span class="model">{daemon.websearchConfig?.provider ?? "web"}</span>
    </button>
    {#if daemon.isBusy}
      <button class="btn-danger" onclick={() => daemon.interrupt()}>
        <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
          <rect x="6" y="6" width="12" height="12"/>
        </svg>
        Interrupt
      </button>
    {/if}
  </div>
</header>

<style>
  .header {
    padding: 14px 24px;
    background-color: var(--bg-header);
    border-bottom: 1px solid var(--border-subtle);
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 12px;
  }

  .menu-btn {
    display: none;
  }

  .session-info {
    min-width: 0;
    flex: 1;
  }

  .title {
    font-size: 16px;
    font-weight: 600;
    color: var(--text-primary);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .meta {
    display: flex;
    gap: 8px;
    margin-top: 2px;
    overflow: hidden;
  }

  .tag {
    font-family: var(--font-mono);
    font-size: 11px;
    color: var(--text-secondary);
    white-space: nowrap;
  }

  .tag.activity {
    color: var(--accent-info);
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .actions {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-shrink: 0;
  }

  .badge.delegated {
    font-family: var(--font-mono);
    font-size: 10px;
    padding: 2px 6px;
    border-radius: var(--radius-sm);
    text-transform: uppercase;
    background: rgba(210, 153, 34, 0.15);
    color: var(--accent-warning);
  }

  .model-btn {
    display: flex;
    align-items: baseline;
    gap: 6px;
    padding: 5px 10px;
    border-radius: var(--radius-md);
    background: var(--bg-surface);
    border: 1px solid var(--border-subtle);
    cursor: pointer;
    max-width: 320px;
  }

  .model-btn:hover {
    border-color: var(--border-strong);
  }

  .model-btn .provider {
    font-size: 10px;
    color: var(--text-muted);
    text-transform: uppercase;
    font-family: var(--font-mono);
  }

  .model-btn .model {
    font-size: 12px;
    color: var(--text-primary);
    font-family: var(--font-mono);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .icon-btn {
    background: transparent;
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-md);
    color: var(--text-secondary);
    width: 30px;
    height: 30px;
    align-items: center;
    justify-content: center;
    cursor: pointer;
    flex-shrink: 0;
  }

  .btn-danger {
    padding: 6px 12px;
    border-radius: var(--radius-md);
    background-color: rgba(248, 81, 73, 0.15);
    color: var(--accent-danger);
    border: 1px solid rgba(248, 81, 73, 0.3);
    font-size: 12px;
    font-weight: 500;
    cursor: pointer;
    display: inline-flex;
    align-items: center;
    gap: 6px;
    transition: background-color 0.15s;
    flex-shrink: 0;
  }

  .btn-danger:hover {
    background-color: rgba(248, 81, 73, 0.25);
  }

  @media (max-width: 900px) {
    .menu-btn {
      display: flex;
    }

    .header {
      padding: 10px 14px;
    }

    .model-btn .provider {
      display: none;
    }
  }
</style>
