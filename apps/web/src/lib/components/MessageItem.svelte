<script lang="ts">
  import type { Message } from "../types.js";
  import { renderMarkdown } from "../markdown.js";
  import Self from "./MessageItem.svelte";

  interface Props {
    message: Message;
  }

  let { message, compact = false }: { message: Message; compact?: boolean } = $props();

  let htmlContent = $derived(
    message.role === "Tool"
      ? ""
      : renderMarkdown(message.display_content ?? message.content ?? ""),
  );

  let timeLabel = $derived.by(() => {
    const ms = message.sent_at_ms ?? (message.timestamp ? message.timestamp * 1000 : null);
    if (!ms) return null;
    return new Date(ms).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  });

  let roleLabel = $derived(
    message.role === "User"
      ? "You"
      : message.role === "Tool"
        ? "tool"
        : message.role === "System"
          ? "notification"
          : "Muta",
  );
</script>

{#if message.role === "Tool"}
  <div class="tool-result-message">
    <span class="role-tag">{roleLabel}</span>
    <pre class="tool-output">{message.content}</pre>
  </div>
{:else}
  <div class="message-bubble {message.role.toLowerCase()}">
    <div class="message-header">
      <span class="role-tag">{roleLabel}</span>
      {#if timeLabel}
        <span class="time-tag">{timeLabel}</span>
      {/if}
    </div>

    {#if message.reasoning_content}
      <details class="reasoning">
        <summary>thinking</summary>
        <pre>{message.reasoning_content}</pre>
      </details>
    {/if}

    {#if message.images && message.images.length > 0}
      <div class="message-images">
        {#each message.images as image, i (i)}
          <img class="message-image" src="data:{image.mime};base64,{image.data}" alt="attached" />
        {/each}
      </div>
    {/if}

    <div class="message-body markdown-body">
      {@html htmlContent}
    </div>

    {#if message.children && message.children.length > 0}
      <details class="runner-children">
        <summary>runner transcript ({message.children.length})</summary>
        <div class="runner-inner">
          {#each message.children as child, i (i)}
            <Self message={child} compact={true} />
          {/each}
        </div>
      </details>
    {/if}
  </div>
{/if}

<style>
  .message-bubble {
    display: flex;
    flex-direction: column;
    margin-bottom: 16px;
    animation: fadeIn 0.15s ease-out;
    content-visibility: auto;
  }

  .message-bubble.user {
    align-self: flex-end;
    background-color: var(--bg-surface);
    border: 1px solid var(--border-subtle);
    padding: 12px 16px;
    border-radius: var(--radius-lg) var(--radius-lg) 2px var(--radius-lg);
    max-width: 80%;
  }

  .message-bubble.assistant,
  .message-bubble.system {
    align-self: flex-start;
    max-width: 90%;
    width: 100%;
  }

  .message-header {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-bottom: 6px;
    font-size: 12px;
  }

  .role-tag {
    font-weight: 600;
    color: var(--text-secondary);
  }

  .time-tag {
    font-size: 11px;
    color: var(--text-muted);
    font-family: var(--font-mono);
  }

  .message-body {
    color: var(--text-primary);
    line-height: 1.6;
    font-size: 14px;
    word-break: break-word;
  }

  .message-images {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    margin-bottom: 8px;
  }

  .message-image {
    max-width: 240px;
    max-height: 180px;
    border-radius: var(--radius-md);
    border: 1px solid var(--border-subtle);
    object-fit: cover;
  }

  .reasoning {
    border-left: 2px solid var(--border-strong);
    padding-left: 10px;
    margin-bottom: 8px;
  }

  .reasoning summary {
    font-size: 11px;
    color: var(--text-muted);
    font-family: var(--font-mono);
    cursor: pointer;
  }

  .reasoning pre {
    font-size: 11px;
    color: var(--text-secondary);
    white-space: pre-wrap;
    max-height: 160px;
    overflow-y: auto;
    margin: 4px 0 0;
  }

  .tool-result-message {
    margin-bottom: 12px;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }

  .tool-output {
    background-color: var(--bg-surface);
    border: 1px solid var(--border-subtle);
    border-radius: var(--radius-md);
    padding: 10px 12px;
    font-family: var(--font-mono);
    font-size: 11px;
    color: var(--text-secondary);
    white-space: pre-wrap;
    word-break: break-all;
    max-height: 200px;
    overflow-y: auto;
    margin: 0;
  }

  .runner-children {
    margin-top: 8px;
    border-left: 2px solid var(--border-strong);
    padding-left: 10px;
  }

  .runner-children summary {
    font-size: 11px;
    color: var(--text-muted);
    cursor: pointer;
    font-family: var(--font-mono);
  }

  .runner-inner {
    padding-top: 6px;
  }

  :global(.markdown-body p) {
    margin-bottom: 10px;
  }

  :global(.markdown-body p:last-child) {
    margin-bottom: 0;
  }

  :global(.markdown-body pre) {
    background-color: var(--bg-surface-hover);
    border: 1px solid var(--border-subtle);
    border-radius: var(--radius-md);
    padding: 12px;
    overflow-x: auto;
    font-family: var(--font-mono);
    font-size: 12px;
    margin: 8px 0;
  }

  :global(.markdown-body code) {
    font-family: var(--font-mono);
    font-size: 12px;
    background-color: rgba(255, 255, 255, 0.06);
    padding: 2px 4px;
    border-radius: var(--radius-sm);
  }

  :global(.markdown-body pre code) {
    background-color: transparent;
    padding: 0;
  }

  @keyframes fadeIn {
    from { opacity: 0; transform: translateY(4px); }
    to { opacity: 1; transform: translateY(0); }
  }
</style>
