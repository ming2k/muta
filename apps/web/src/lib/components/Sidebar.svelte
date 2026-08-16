<script lang="ts">
  import { daemon } from "../stores/daemon.svelte.js";
  import type { MonitoredSession } from "../types.js";

  interface Props {
    open: boolean;
    onClose: () => void;
    onOpenConnection: () => void;
  }

  let { open, onClose, onOpenConnection }: Props = $props();

  /** Session id pending a second click to confirm deletion. */
  let confirmDeleteId = $state<string | null>(null);
  /** Session id whose title is being edited inline. */
  let editingId = $state<string | null>(null);
  let editValue = $state("");

  const statusLabels: Record<string, string> = {
    idle: "idle",
    running: "running",
    needs_approval: "approval",
    needs_input: "input",
    interrupted: "stopped",
    failed: "failed",
  };

  function sessionTitle(s: MonitoredSession): string {
    return s.overview || s.id.slice(0, 8);
  }

  function select(id: string) {
    if (editingId === id) return;
    daemon.attach(id);
    onClose();
  }

  function startRename(s: MonitoredSession) {
    editingId = s.id;
    editValue = s.overview ?? "";
  }

  /** Svelte action: focus the inline rename input on mount. */
  function focusOnMount(el: HTMLInputElement) {
    el.focus();
    el.select();
  }

  function commitRename(id: string) {
    const title = editValue.trim();
    if (title) daemon.renameSession(id, title);
    editingId = null;
  }

  function requestDelete(id: string) {
    if (confirmDeleteId === id) {
      daemon.deleteSession(id);
      confirmDeleteId = null;
    } else {
      confirmDeleteId = id;
      window.setTimeout(() => {
        if (confirmDeleteId === id) confirmDeleteId = null;
      }, 4000);
    }
  }
</script>

{#if open}
  <div class="backdrop" onclick={onClose} role="presentation"></div>
{/if}

<aside class="sidebar" class:open>
  <div class="brand-header">
    <div class="brand-logo">
      <span class="dot" class:online={daemon.connection === "connected"}></span>
      <span class="title">neenee</span>
    </div>
    <button
      class="badge"
      class:online={daemon.connection === "connected"}
      onclick={onOpenConnection}
      title="Connection settings"
    >
      {daemon.connection === "connected" ? "Online" : daemon.connection === "connecting" ? "Connecting" : "Offline"}
    </button>
  </div>

  <div class="action-bar">
    <button class="btn-primary" onclick={() => daemon.newSession()}>
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
          <div
            class="session-item"
            class:active={s.id === daemon.activeSessionId}
            role="button"
            tabindex="0"
            onclick={() => select(s.id)}
            onkeydown={(e) => e.key === "Enter" && select(s.id)}
          >
            <div class="session-header">
              {#if editingId === s.id}
                <input
                  class="rename-input"
                  bind:value={editValue}
                  onkeydown={(e) => {
                    if (e.key === "Enter") commitRename(s.id);
                    if (e.key === "Escape") editingId = null;
                    e.stopPropagation();
                  }}
                  onclick={(e) => e.stopPropagation()}
                  onfocusout={() => commitRename(s.id)}
                  aria-label="Rename session"
                  use:focusOnMount
                />
              {:else}
                <span class="session-title">{sessionTitle(s)}</span>
              {/if}
              <span class="status-pill status-{s.status}">
                {statusLabels[s.status] ?? s.status}
              </span>
            </div>
            <div class="session-meta">
              <span class="activity">{s.current_tool ?? s.activity ?? ""}</span>
              <span class="meta-right">
                {#if s.context_tokens}
                  <span>{s.context_tokens.toLocaleString()} tok</span>
                {/if}
                <button
                  class="icon-action rename-btn"
                  title="Rename session"
                  onclick={(e) => {
                    e.stopPropagation();
                    startRename(s);
                  }}
                >
                  ✎
                </button>
                <button
                  class="icon-action delete-btn"
                  class:confirm={confirmDeleteId === s.id}
                  title={confirmDeleteId === s.id ? "Click again to confirm deletion" : "Delete session"}
                  onclick={(e) => {
                    e.stopPropagation();
                    requestDelete(s.id);
                  }}
                >
                  {confirmDeleteId === s.id ? "confirm?" : "×"}
                </button>
              </span>
            </div>
          </div>
        {/each}
      {/if}
    </div>
  </div>

  <div class="sidebar-footer">
    <span class="footer-text">{daemon.daemonProjectRoot || daemon.wsUrl}</span>
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

  .backdrop {
    display: none;
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
    border: none;
    cursor: pointer;
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
    gap: 6px;
  }

  .session-title {
    font-weight: 500;
    font-size: 13px;
    color: var(--text-primary);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    max-width: 150px;
  }

  .status-pill {
    font-family: var(--font-mono);
    font-size: 10px;
    padding: 1px 5px;
    border-radius: var(--radius-sm);
    text-transform: uppercase;
    flex-shrink: 0;
  }

  .status-idle {
    background: var(--bg-surface-hover);
    color: var(--text-muted);
  }

  .status-running {
    background: rgba(88, 166, 255, 0.15);
    color: var(--accent-info);
  }

  .status-needs_approval,
  .status-needs_input {
    background: rgba(210, 153, 34, 0.15);
    color: var(--accent-warning);
  }

  .status-interrupted {
    background: var(--bg-surface-hover);
    color: var(--text-secondary);
  }

  .status-failed {
    background: rgba(248, 81, 73, 0.15);
    color: var(--accent-danger);
  }

  .session-meta {
    font-size: 11px;
    color: var(--text-muted);
    display: flex;
    justify-content: space-between;
    gap: 8px;
    align-items: center;
  }

  .activity {
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .meta-right {
    display: flex;
    align-items: center;
    gap: 6px;
    flex-shrink: 0;
  }

  .icon-action {
    background: transparent;
    border: none;
    color: var(--text-muted);
    font-size: 13px;
    line-height: 1;
    cursor: pointer;
    padding: 0 2px;
    opacity: 0;
    transition: opacity 0.15s;
  }

  .session-item:hover .icon-action,
  .delete-btn.confirm {
    opacity: 1;
  }

  .icon-action:hover {
    color: var(--text-secondary);
  }

  .delete-btn:hover {
    color: var(--accent-danger) !important;
  }

  .delete-btn.confirm {
    color: var(--accent-danger);
    font-size: 10px;
    font-family: var(--font-mono);
    border: 1px solid rgba(248, 81, 73, 0.4);
    border-radius: var(--radius-sm);
    padding: 1px 4px;
  }

  .rename-input {
    flex: 1;
    min-width: 0;
    background: var(--input-bg-inactive);
    border: 1px solid var(--border-input-focus);
    border-radius: var(--radius-sm);
    color: var(--text-primary);
    font-size: 13px;
    padding: 2px 6px;
    outline: none;
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
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  @media (max-width: 900px) {
    .sidebar {
      position: fixed;
      top: 0;
      left: 0;
      bottom: 0;
      z-index: 90;
      transform: translateX(-100%);
      transition: transform 0.2s ease-out;
      box-shadow: 8px 0 32px rgba(0, 0, 0, 0.4);
    }

    .sidebar.open {
      transform: translateX(0);
    }

    .backdrop {
      display: block;
      position: fixed;
      inset: 0;
      background: rgba(0, 0, 0, 0.45);
      z-index: 80;
    }
  }
</style>
