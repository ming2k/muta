<script lang="ts">
  import { daemon } from "../stores/daemon.svelte.js";
  import type { ProviderPickerRow } from "../types.js";

  interface Props {
    onclose: () => void;
  }

  let { onclose }: Props = $props();

  interface ModelEntry {
    id: string;
    provider: ProviderPickerRow;
    favorite: boolean;
    effort: string | null;
    thinking: boolean | null;
    active: boolean;
  }

  /** Flatten the picker snapshot into one row per served model. */
  let entries = $derived.by((): ModelEntry[] => {
    const snapshot = daemon.providerPicker;
    if (!snapshot) return [];
    const out: ModelEntry[] = [];
    for (const row of snapshot.rows) {
      for (const model of row.models) {
        const info = row.model_info?.find((m) => m.model === model);
        out.push({
          id: model,
          provider: row,
          favorite: info?.favorite ?? false,
          effort: info?.effort ?? null,
          thinking: info?.thinking ?? null,
          active: row.id === snapshot.default_id && row.model === model,
        });
      }
    }
    // Two-tier ordering, mirroring the TUI's Models picker: the live
    // (provider, model) pair first, favorites next, everything else after —
    // each tier ASCII-sorted by the model id with the provider label as the
    // tiebreaker. ASCII (not localeCompare) keeps the web and TUI lists in
    // the same order.
    return out.sort((a, b) => {
      const weight = (e: ModelEntry) => (e.active ? 2 : e.favorite ? 1 : 0);
      return (
        weight(b) - weight(a) ||
        (a.id < b.id ? -1 : a.id > b.id ? 1 : 0) ||
        (a.provider.name < b.provider.name ? -1 : a.provider.name > b.provider.name ? 1 : 0)
      );
    });
  });

  function choose(entry: ModelEntry) {
    if (!entry.provider.key_ready) return;
    daemon.setDefaultModel(entry.id);
    onclose();
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
  <div class="modal" role="dialog" aria-label="Choose model">
    <div class="modal-header">
      <h3>Models</h3>
      <button class="close" aria-label="Close" onclick={onclose}>×</button>
    </div>

    <div class="modal-body">
      {#if !daemon.providerPicker}
        <div class="empty">Model list not available — attach to a session first.</div>
      {:else if entries.length === 0}
        <div class="empty">No providers configured.</div>
      {:else}
        {#each entries as entry (entry.provider.id + ":" + entry.id)}
          <button
            class="model-row"
            class:active={entry.active}
            class:unavailable={!entry.provider.key_ready}
            onclick={() => choose(entry)}
            title={entry.provider.key_ready
              ? `${entry.provider.name} — click to switch`
              : `${entry.provider.name} — no API key configured`}
          >
            <span class="star" class:favorite={entry.favorite}>{entry.favorite ? "★" : ""}</span>
            <span class="model-name">{entry.id}</span>
            <span class="provider-name">{entry.provider.name}</span>
            <span class="flags">
              {#if entry.effort}
                <span class="flag">effort: {entry.effort}</span>
              {/if}
              {#if entry.thinking}
                <span class="flag">thinking</span>
              {/if}
              {#if !entry.provider.key_ready}
                <span class="flag no-key">no key</span>
              {/if}
              {#if entry.active}
                <span class="flag current">current</span>
              {/if}
            </span>
          </button>
        {/each}
      {/if}
    </div>

    <div class="modal-footer">
      Switching sets the default model, mirroring the TUI's Models picker.
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
    padding-top: 12vh;
    z-index: 100;
  }

  .modal {
    width: 560px;
    max-width: calc(100vw - 32px);
    max-height: 70vh;
    background-color: var(--bg-sidebar);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-lg);
    box-shadow: 0 12px 40px rgba(0, 0, 0, 0.5);
    display: flex;
    flex-direction: column;
    overflow: hidden;
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
    overflow-y: auto;
    padding: 8px;
  }

  .empty {
    padding: 24px;
    text-align: center;
    color: var(--text-muted);
    font-size: 12px;
  }

  .model-row {
    width: 100%;
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 9px 12px;
    background: transparent;
    border: 1px solid transparent;
    border-radius: var(--radius-md);
    cursor: pointer;
    text-align: left;
  }

  .model-row:hover {
    background: var(--bg-surface);
  }

  .model-row.active {
    border-color: var(--border-strong);
    background: var(--bg-surface);
  }

  .model-row.unavailable {
    opacity: 0.5;
    cursor: not-allowed;
  }

  .star {
    width: 14px;
    color: var(--accent-warning);
    flex-shrink: 0;
  }

  .model-name {
    font-family: var(--font-mono);
    font-size: 13px;
    color: var(--text-primary);
    font-weight: 500;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .provider-name {
    font-size: 11px;
    color: var(--text-muted);
    flex-shrink: 0;
  }

  .flags {
    margin-left: auto;
    display: flex;
    gap: 6px;
    flex-shrink: 0;
  }

  .flag {
    font-family: var(--font-mono);
    font-size: 10px;
    padding: 1px 5px;
    border-radius: var(--radius-sm);
    background: var(--bg-surface-hover);
    color: var(--text-muted);
  }

  .flag.no-key {
    color: var(--accent-danger);
    background: rgba(248, 81, 73, 0.15);
  }

  .flag.current {
    color: var(--accent-primary);
    background: rgba(46, 160, 67, 0.15);
  }

  .modal-footer {
    padding: 10px 16px;
    border-top: 1px solid var(--border-subtle);
    font-size: 11px;
    color: var(--text-muted);
  }
</style>
