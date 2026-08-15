<script lang="ts">
  import { daemon } from "../stores/daemon.svelte.js";
</script>

<header class="header">
  <div class="session-info">
    <h2 class="title">{daemon.activeSession?.title || daemon.activeSessionId || "No active session"}</h2>
    <div class="meta">
      {#if daemon.activeSession}
        <span class="tag">{daemon.activeSession.provider} / {daemon.activeSession.model}</span>
        {#if daemon.activeSession.context_tokens}
          <span class="tag">{daemon.activeSession.context_tokens.toLocaleString()} tokens</span>
        {/if}
      {/if}
    </div>
  </div>

  <div class="actions">
    {#if daemon.isBusy}
      <button class="btn btn-danger" onclick={() => daemon.interrupt()}>
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
    background-color: var(--bg-sidebar);
    border-bottom: 1px solid var(--border-subtle);
    display: flex;
    justify-content: space-between;
    align-items: center;
  }

  .title {
    font-size: 16px;
    font-weight: 600;
    color: var(--text-primary);
  }

  .meta {
    display: flex;
    gap: 8px;
    margin-top: 2px;
  }

  .tag {
    font-family: var(--font-mono);
    font-size: 11px;
    color: var(--text-secondary);
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
  }

  .btn-danger:hover {
    background-color: rgba(248, 81, 73, 0.25);
  }
</style>
