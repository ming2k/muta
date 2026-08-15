<script lang="ts">
  import { daemon } from "../stores/daemon.svelte.js";

  let inputVal = $state("");
  let textareaEl: HTMLTextAreaElement;

  function handleSend() {
    const text = inputVal.trim();
    if (!text || daemon.isBusy) return;
    daemon.sendChat(text);
    inputVal = "";
    if (textareaEl) {
      textareaEl.style.height = "auto";
    }
  }

  function handleKeyDown(e: KeyboardEvent) {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      handleSend();
    }
  }

  function handleInput() {
    if (textareaEl) {
      textareaEl.style.height = "auto";
      textareaEl.style.height = Math.min(textareaEl.scrollHeight, 180) + "px";
    }
  }

  function insertCommand(cmd: string) {
    inputVal = cmd;
    if (textareaEl) {
      textareaEl.focus();
    }
  }
</script>

<footer class="composer-container">
  <div class="composer-box">
    <textarea
      bind:this={textareaEl}
      bind:value={inputVal}
      onkeydown={handleKeyDown}
      oninput={handleInput}
      placeholder="Type your message, or ask coding tasks... (Enter to send, Shift+Enter for newline)"
      rows="1"
      disabled={daemon.isBusy}
    ></textarea>

    <div class="toolbar">
      <div class="hints">
        <button class="hint-pill" onclick={() => insertCommand("/help")}>/help</button>
        <button class="hint-pill" onclick={() => insertCommand("/status")}>/status</button>
        <button class="hint-pill" onclick={() => insertCommand("/mcp")}>/mcp</button>
      </div>

      <button
        class="send-btn"
        aria-label="Send message"
        disabled={!inputVal.trim() || daemon.isBusy}
        onclick={handleSend}
      >
        <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
          <path d="M22 2L11 13M22 2l-7 20-4-9-9-4 20-7z"/>
        </svg>
      </button>
    </div>
  </div>
</footer>

<style>
  .composer-container {
    padding: 16px 24px 20px;
    background-color: var(--bg-app);
  }

  .composer-box {
    background-color: var(--bg-input);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-lg);
    padding: 12px 16px 8px;
    display: flex;
    flex-direction: column;
    transition: border-color 0.15s;
  }

  .composer-box:focus-within {
    border-color: var(--accent-info);
  }

  textarea {
    background: transparent;
    border: none;
    color: var(--text-primary);
    font-family: var(--font-sans);
    font-size: 14px;
    resize: none;
    outline: none;
    min-height: 24px;
    max-height: 180px;
    line-height: 1.5;
  }

  textarea:disabled {
    opacity: 0.6;
    cursor: not-allowed;
  }

  .toolbar {
    display: flex;
    justify-content: space-between;
    align-items: center;
    margin-top: 8px;
  }

  .hints {
    display: flex;
    gap: 6px;
  }

  .hint-pill {
    font-family: var(--font-mono);
    font-size: 11px;
    padding: 2px 6px;
    border-radius: var(--radius-sm);
    background-color: var(--bg-surface);
    color: var(--text-muted);
    border: none;
    cursor: pointer;
    transition: all 0.15s;
  }

  .hint-pill:hover {
    background-color: var(--bg-surface-hover);
    color: var(--text-secondary);
  }

  .send-btn {
    width: 32px;
    height: 32px;
    border-radius: 50%;
    background-color: var(--accent-primary);
    color: #fff;
    border: none;
    display: flex;
    align-items: center;
    justify-content: center;
    cursor: pointer;
    transition: opacity 0.15s;
  }

  .send-btn:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }

  .send-btn:not(:disabled):hover {
    opacity: 0.9;
  }
</style>
