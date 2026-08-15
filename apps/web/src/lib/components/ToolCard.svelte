<script lang="ts">
  import type { LiveToolExecution } from "../types.js";

  interface Props {
    tool: LiveToolExecution;
  }

  let { tool }: Props = $props();
  let expanded = $state(true);
</script>

<div class="tool-card">
  <button class="tool-header" onclick={() => (expanded = !expanded)}>
    <div class="tool-title">
      <span class="icon">⚡</span>
      <span class="name">{tool.name}</span>
    </div>
    <div class="tool-badge status-{tool.status}">
      {tool.status === "running" ? "running..." : `done (${tool.durationMs || 0}ms)`}
    </div>
  </button>

  {#if expanded}
    <div class="tool-content">
      <div class="block">
        <div class="label">Arguments</div>
        <pre>{tool.arguments}</pre>
      </div>
      {#if tool.output}
        <div class="block">
          <div class="label">Result</div>
          <pre class="output">{tool.output}</pre>
        </div>
      {/if}
    </div>
  {/if}
</div>

<style>
  .tool-card {
    background-color: var(--bg-surface);
    border: 1px solid var(--border-subtle);
    border-radius: var(--radius-md);
    margin: 8px 0;
    overflow: hidden;
  }

  .tool-header {
    width: 100%;
    padding: 8px 12px;
    background: transparent;
    border: none;
    display: flex;
    justify-content: space-between;
    align-items: center;
    cursor: pointer;
    text-align: left;
    font-family: var(--font-mono);
    font-size: 12px;
  }

  .tool-title {
    display: flex;
    align-items: center;
    gap: 6px;
  }

  .name {
    font-weight: 600;
    color: var(--accent-info);
  }

  .tool-badge {
    font-size: 11px;
    padding: 1px 6px;
    border-radius: var(--radius-sm);
  }

  .status-running {
    color: var(--accent-warning);
  }

  .status-completed {
    color: var(--accent-primary);
  }

  .status-failed {
    color: var(--accent-danger);
  }

  .tool-content {
    padding: 10px 12px;
    background-color: var(--bg-input);
    border-top: 1px solid var(--border-subtle);
    font-family: var(--font-mono);
    font-size: 11px;
    max-height: 240px;
    overflow-y: auto;
  }

  .block {
    margin-bottom: 8px;
  }

  .block:last-child {
    margin-bottom: 0;
  }

  .label {
    color: var(--text-muted);
    font-size: 10px;
    text-transform: uppercase;
    margin-bottom: 2px;
  }

  pre {
    margin: 0;
    white-space: pre-wrap;
    word-break: break-all;
    color: var(--text-secondary);
  }

  pre.output {
    color: var(--text-primary);
  }
</style>
