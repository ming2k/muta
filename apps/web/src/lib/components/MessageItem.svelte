<script lang="ts">
  import { marked } from "marked";
  import type { Message } from "../types.js";

  interface Props {
    message: Message;
  }

  let { message }: Props = $props();

  let htmlContent = $derived(
    marked.parse(message.content || "", { breaks: true, gfm: true })
  );
</script>

<div class="message-bubble {message.role}">
  <div class="message-header">
    <span class="role-tag">{message.role === "user" ? "You" : "Neenee"}</span>
    {#if message.timestamp}
      <span class="time-tag">{new Date(message.timestamp).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })}</span>
    {/if}
  </div>

  <div class="message-body markdown-body">
    {@html htmlContent}
  </div>
</div>

<style>
  .message-bubble {
    display: flex;
    flex-direction: column;
    margin-bottom: 16px;
    animation: fadeIn 0.15s ease-out;
  }

  .message-bubble.user {
    align-self: flex-end;
    background-color: var(--bg-surface);
    border: 1px solid var(--border-subtle);
    padding: 12px 16px;
    border-radius: var(--radius-lg) var(--radius-lg) 2px var(--radius-lg);
    max-width: 80%;
  }

  .message-bubble.assistant {
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

  :global(.markdown-body p) {
    margin-bottom: 10px;
  }

  :global(.markdown-body p:last-child) {
    margin-bottom: 0;
  }

  :global(.markdown-body pre) {
    background-color: var(--bg-input);
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

  @keyframes fadeIn {
    from { opacity: 0; transform: translateY(4px); }
    to { opacity: 1; transform: translateY(0); }
  }
</style>
