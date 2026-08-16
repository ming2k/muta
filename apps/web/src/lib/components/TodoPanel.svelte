<script lang="ts">
  import { daemon } from "../stores/daemon.svelte.js";

  let collapsed = $state(false);

  let items = $derived(daemon.todos.items);
  let doneCount = $derived(
    items.filter((i) => i.status === "completed" || i.status === "cancelled").length,
  );

  function glyph(status: string): string {
    switch (status) {
      case "completed":
        return "✓";
      case "in_progress":
        return "◐";
      case "cancelled":
        return "✕";
      default:
        return "○";
    }
  }
</script>

{#if items.length > 0}
  <div class="todo-panel">
    <button class="todo-header" onclick={() => (collapsed = !collapsed)}>
      <span class="title">Tasks</span>
      <span class="progress">{doneCount}/{items.length}</span>
      <span class="chevron">{collapsed ? "▸" : "▾"}</span>
    </button>
    {#if !collapsed}
      <ul class="todo-list">
        {#each items as item (item.id)}
          <li class="todo status-{item.status}">
            <span class="glyph">{glyph(item.status)}</span>
            <span class="content">{item.content}</span>
          </li>
        {/each}
      </ul>
    {/if}
  </div>
{/if}

<style>
  .todo-panel {
    margin: 0 24px;
    border: 1px solid var(--border-subtle);
    border-radius: var(--radius-md);
    background-color: var(--bg-surface);
    overflow: hidden;
    flex-shrink: 0;
  }

  .todo-header {
    width: 100%;
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 6px 12px;
    background: transparent;
    border: none;
    cursor: pointer;
    font-family: var(--font-mono);
    font-size: 11px;
    color: var(--text-secondary);
    text-transform: uppercase;
    letter-spacing: 0.5px;
  }

  .todo-header .title {
    font-weight: 600;
  }

  .todo-header .progress {
    color: var(--text-muted);
  }

  .chevron {
    margin-left: auto;
    color: var(--text-muted);
    font-size: 10px;
  }

  .todo-list {
    list-style: none;
    max-height: 160px;
    overflow-y: auto;
    border-top: 1px solid var(--border-subtle);
    padding: 6px 0;
  }

  .todo {
    display: flex;
    align-items: baseline;
    gap: 8px;
    padding: 3px 12px;
    font-size: 12px;
    color: var(--text-primary);
  }

  .glyph {
    font-family: var(--font-mono);
    flex-shrink: 0;
    color: var(--text-muted);
  }

  .status-completed .glyph {
    color: var(--accent-primary);
  }

  .status-completed .content {
    color: var(--text-muted);
    text-decoration: line-through;
  }

  .status-in_progress .glyph {
    color: var(--accent-info);
  }

  .status-cancelled .content {
    color: var(--text-muted);
    text-decoration: line-through;
  }

  .content {
    word-break: break-word;
  }
</style>
