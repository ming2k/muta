<script lang="ts">
  import { daemon } from "../stores/daemon.svelte.js";
</script>

<aside class="sidebar">
  <div class="brand-header">
    <div class="brand-logo">
      <span class="dot" class:online={daemon.connected}></span>
      <span class="title">neenee</span>
    </div>
    <span class="badge" class:online={daemon.connected}>
      {daemon.connected ? "Online" : "Connecting"}
    </span>
  </div>

  <div class="action-bar">
    <button class="btn btn-primary" onclick={() => daemon.newSession()}>
      <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
        <path d="M12 5v14M5 12h14"/>
      </svg>
      New Session
    </button>
  </div>

  <div class="sessions-container">
    <div class="section-title">Hosted Sessions ({daemon.sessions.length})</div>
    <div class="session-list">
      {#if daemon.sessions.length === 0}
        <div class="empty">No active sessions</div>
      {:else}
        {#each daemon.sessions as s (s.id)}
          <button
            class="session-item"
            class:active={s.id === daemon.activeSessionId}
            onclick={() => daemon.attach(s.id)}
          >
            <div class="session-header">
              <span class="session-title">{s.title || s.id.slice(0, 8)}</span>
              <span class="status-pill status-{s.status}">{s.status}</span>
            </div>
            <div class="session-meta">
              <span>{s.provider} / {s.model}</span>
              {#if s.context_tokens}
                <span>{s.context_tokens.toLocaleString()} tok</span>
              {/if}
            </div>
          </button>
        {/each}
      {/if}
    </div>
  </div>

  <div class="sidebar-footer">
    <span class="footer-text">{daemon.wsUrl}</span>
  </div>
</aside>

<style>
  .sidebar {
    width: 280px;
    height: 100%;
    background-color: var(--bg-sidebar);
    border-right: 1px solid var(--border-subtle);
    display: flex;
    flex-direction: column;
    flex-shrink: 0;
  }

  .brand-header {
    padding: 16px 20px;
    display: flex;
    justify-content: space-between;
    align-items: center;
    border-bottom: 1px solid var(--border-subtle);
  }

  .brand-logo {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background-color: var(--accent-danger);
    transition: background-color 0.3s;
  }

  .dot.online {
    background-color: var(--accent-primary);
  }

  .title {
    font-family: var(--font-mono);
    font-weight: 600;
    font-size: 16px;
    letter-spacing: -0.5px;
  }

  .badge {
    font-family: var(--font-mono);
    font-size: 11px;
    padding: 2px 6px;
    border-radius: var(--radius-sm);
    background: rgba(248, 81, 73, 0.15);
    color: var(--accent-danger);
    text-transform: uppercase;
  }

  .badge.online {
    background: rgba(46, 160, 67, 0.15);
    color: var(--accent-primary);
  }

  .action-bar {
    padding: 14px 16px 8px;
  }

  .btn-primary {
    width: 100%;
    padding: 8px 12px;
    background-color: var(--accent-primary);
    color: #fff;
    border: none;
    border-radius: var(--radius-md);
    font-size: 13px;
    font-weight: 500;
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 6px;
    cursor: pointer;
    transition: opacity 0.15s;
  }

  .btn-primary:hover {
    opacity: 0.9;
  }

  .sessions-container {
    flex: 1;
    overflow-y: auto;
    padding: 8px 12px;
  }

  .section-title {
    font-size: 11px;
    text-transform: uppercase;
    font-weight: 600;
    color: var(--text-muted);
    letter-spacing: 0.5px;
    margin: 8px 4px;
  }

  .session-list {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }

  .session-item {
    padding: 10px 12px;
    background: transparent;
    border: 1px solid transparent;
    border-radius: var(--radius-md);
    cursor: pointer;
    text-align: left;
    transition: background-color 0.15s;
  }

  .session-item:hover {
    background: var(--bg-surface);
  }

  .session-item.active {
    background: var(--bg-surface);
    border-color: var(--border-strong);
  }

  .session-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    margin-bottom: 4px;
  }

  .session-title {
    font-weight: 500;
    font-size: 13px;
    color: var(--text-primary);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    max-width: 160px;
  }

  .status-pill {
    font-family: var(--font-mono);
    font-size: 10px;
    padding: 1px 5px;
    border-radius: var(--radius-sm);
    text-transform: uppercase;
  }

  .status-idle {
    background: var(--bg-input);
    color: var(--text-muted);
  }

  .status-running {
    background: rgba(88, 166, 255, 0.15);
    color: var(--accent-info);
  }

  .session-meta {
    font-size: 11px;
    color: var(--text-muted);
    display: flex;
    justify-content: space-between;
  }

  .empty {
    padding: 24px;
    text-align: center;
    color: var(--text-muted);
    font-size: 12px;
  }

  .sidebar-footer {
    padding: 12px 16px;
    border-top: 1px solid var(--border-subtle);
    font-family: var(--font-mono);
    font-size: 11px;
    color: var(--text-muted);
  }
</style>
