<script lang="ts">
  import type { RunnerExecution, LiveToolExecution } from "../stores/daemon.svelte.js";

  interface Props {
    tool: LiveToolExecution;
  }

  let { tool }: Props = $props();
  let expanded = $state(true);
  let runnerExpanded = $state(true);

  let statusLabel = $derived.by(() => {
    switch (tool.status) {
      case "running":
        return "running…";
      case "failed":
        return `failed (${tool.durationMs ?? 0}ms)`;
      case "cancelled":
        return "cancelled";
      default:
        return `done (${tool.durationMs ?? 0}ms)`;
    }
  });

  let livePreview = $derived(
    tool.status === "running" ? tool.stdout || tool.stderr : "",
  );

  function runnerSummary(runner: RunnerExecution): string {
    const parts: string[] = [];
    if (runner.profile) parts.push(runner.profile);
    if (runner.activity) parts.push(runner.activity);
    const running = runner.tools.filter((t) => t.status === "running").length;
    if (running > 0) parts.push(`${running} tool${running > 1 ? "s" : ""} running`);
    return parts.join(" · ") || "working…";
  }
</script>

<div class="tool-card">
  <button class="tool-header" onclick={() => (expanded = !expanded)}>
    <div class="tool-title">
      <span class="icon">⚡</span>
      <span class="name">{tool.name}</span>
    </div>
    <div class="tool-badge status-{tool.status}">{statusLabel}</div>
  </button>

  {#if expanded}
    <div class="tool-content">
      <div class="block">
        <div class="label">Arguments</div>
        <pre>{tool.arguments}</pre>
      </div>
      {#if tool.status === "running" && livePreview}
        <div class="block">
          <div class="label">Live output</div>
          <pre class="stream">{livePreview}</pre>
        </div>
      {/if}
      {#if tool.stdout}
        <div class="block">
          <div class="label">stdout</div>
          <pre>{tool.stdout}</pre>
        </div>
      {/if}
      {#if tool.stderr}
        <div class="block">
          <div class="label">stderr</div>
          <pre class="err">{tool.stderr}</pre>
        </div>
      {/if}
      {#if tool.output}
        <div class="block">
          <div class="label">Result</div>
          <pre class="output">{tool.output}</pre>
        </div>
      {/if}

      {#if tool.runner}
        {@const runner = tool.runner}
        <div class="runner-block">
          <button class="runner-header" onclick={() => (runnerExpanded = !runnerExpanded)}>
            <span class="runner-icon">⎇</span>
            <span class="runner-title">runner — {runnerSummary(runner)}</span>
            <span class="chevron">{runnerExpanded ? "-" : "+"}</span>
          </button>
          {#if runnerExpanded}
            <div class="runner-content">
              {#each runner.tools as sub (sub.id)}
                <div class="runner-tool">
                  <div class="runner-tool-head">
                    <span class="name">{sub.name}</span>
                    <span class="sub-status status-{sub.status}">
                      {sub.status === "running" ? "running…" : `done (${sub.durationMs ?? 0}ms)`}
                    </span>
                  </div>
                  {#if sub.output}
                    <pre>{sub.output}</pre>
                  {/if}
                </div>
              {/each}
              {#if runner.streamingReasoning}
                <details class="runner-reasoning" open>
                  <summary>thinking…</summary>
                  <pre class="runner-reasoning-text">{runner.streamingReasoning}</pre>
                </details>
              {/if}
              {#each runner.reasoning as trace, i (i)}
                <details class="runner-reasoning">
                  <summary>thinking</summary>
                  <pre class="runner-reasoning-text">{trace}</pre>
                </details>
              {/each}
              {#if runner.streamingText}
                <pre class="runner-stream">{runner.streamingText}</pre>
              {/if}
              {#if runner.text}
                <pre class="runner-text">{runner.text}</pre>
              {/if}
            </div>
          {/if}
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
    content-visibility: auto;
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

  .status-cancelled {
    color: var(--text-muted);
  }

  .tool-content {
    padding: 10px 12px;
    background-color: var(--bg-surface);
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

  pre.err {
    color: var(--accent-danger);
  }

  pre.output {
    color: var(--text-primary);
  }

  .runner-block {
    margin-top: 8px;
    border-left: 2px solid var(--border-strong);
    padding-left: 10px;
  }

  .runner-header {
    display: flex;
    align-items: center;
    gap: 6px;
    width: 100%;
    background: transparent;
    border: none;
    cursor: pointer;
    padding: 2px 0;
    font-family: var(--font-mono);
    font-size: 11px;
    text-align: left;
  }

  .runner-icon {
    color: var(--accent-warning);
  }

  .runner-title {
    color: var(--text-muted);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .chevron {
    margin-left: auto;
    color: var(--text-muted);
    font-size: 10px;
    flex-shrink: 0;
  }

  .runner-content {
    padding: 6px 0 2px;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }

  .runner-tool {
    background: var(--bg-surface-hover);
    border-radius: var(--radius-sm);
    padding: 6px 8px;
  }

  .runner-tool-head {
    display: flex;
    justify-content: space-between;
    gap: 8px;
  }

  .runner-tool pre {
    margin-top: 4px;
    max-height: 120px;
    overflow-y: auto;
  }

  .sub-status {
    font-size: 10px;
    flex-shrink: 0;
  }

  .runner-stream {
    color: var(--text-muted);
    max-height: 140px;
    overflow-y: auto;
  }

  .runner-reasoning summary {
    cursor: pointer;
    color: var(--text-muted);
    font-size: 11px;
    padding: 2px 0;
    user-select: none;
  }

  .runner-reasoning-text {
    color: var(--text-muted);
    max-height: 160px;
    overflow-y: auto;
  }

  .runner-text {
    color: var(--text-secondary);
    max-height: 200px;
    overflow-y: auto;
  }
</style>
