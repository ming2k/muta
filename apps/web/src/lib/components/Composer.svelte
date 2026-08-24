<script lang="ts">
  import { daemon } from "../stores/daemon.svelte.js";
  import type { ImagePart, InputCompletion } from "../types.js";

  interface PendingImage {
    part: ImagePart;
    /** Object URL for the thumbnail preview. */
    previewUrl: string;
  }

  let inputVal = $state("");
  let textareaEl: HTMLTextAreaElement;
  let fileInputEl: HTMLInputElement;
  let images = $state<PendingImage[]>([]);
  let completionMatches = $derived(
    daemon.inputCompletions
      .filter(
        (item) =>
          !(
            item.replace_start === 0 &&
            item.label === inputVal &&
            item.replace_end === Array.from(inputVal).length
          ),
      )
      .slice(0, 6),
  );

  const MAX_IMAGE_BYTES = 10 * 1024 * 1024;

  // Restore a prompt the daemon reports as never-sent (UnsentInput) so the
  // user can re-edit and re-send instead of retyping. The daemon only sets
  // `restoredDraft` when this composer reported itself idle, so the
  // asynchronous restore never clobbers in-progress typing (mirrors the
  // TUI's `DraftAdoption::OnlyIfIdle`).
  $effect(() => {
    const draft = daemon.restoredDraft;
    if (!draft) return;
    daemon.takeRestoredDraft();
    inputVal = draft.text;
    clearImages();
    for (const part of draft.images) {
      images.push({ part, previewUrl: `data:${part.mime};base64,${part.data}` });
    }
    resize();
  });

  // Report composer idleness so the daemon's UnsentInput handler can decide
  // between adopting the restored draft and keeping in-progress typing.
  $effect(() => {
    daemon.composerIdle = inputVal.length === 0 && images.length === 0;
  });

  function resize() {
    if (textareaEl) {
      textareaEl.style.height = "auto";
      textareaEl.style.height = Math.min(textareaEl.scrollHeight, 180) + "px";
    }
  }

  function clearImages() {
    for (const img of images) {
      if (img.previewUrl.startsWith("blob:")) URL.revokeObjectURL(img.previewUrl);
    }
    images = [];
  }

  function handleSend() {
    const text = inputVal.trim();
    if ((!text && images.length === 0) || !daemon.sessionAttached) return;
    daemon.sendChat(
      text,
      images.map((i) => i.part),
    );
    inputVal = "";
    daemon.requestInputCompletions("", 0);
    clearImages();
    resize();
  }

  function handleKeyDown(e: KeyboardEvent) {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      handleSend();
    }
  }

  function handleInput() {
    resize();
    daemon.requestInputCompletions(inputVal, textareaEl?.selectionStart ?? inputVal.length);
  }

  function readFile(file: File): Promise<PendingImage | null> {
    return new Promise((resolve) => {
      if (!file.type.startsWith("image/")) return resolve(null);
      if (file.size > MAX_IMAGE_BYTES) {
        daemon.pushToast("warning", "Image too large", `${file.name} exceeds 10 MB.`);
        return resolve(null);
      }
      const reader = new FileReader();
      reader.onload = () => {
        const dataUrl = reader.result as string;
        const base64 = dataUrl.slice(dataUrl.indexOf(",") + 1);
        resolve({
          part: { mime: file.type, data: base64 },
          previewUrl: URL.createObjectURL(file),
        });
      };
      reader.onerror = () => resolve(null);
      reader.readAsDataURL(file);
    });
  }

  async function addFiles(files: Iterable<File>) {
    for (const file of files) {
      const pending = await readFile(file);
      if (pending) images.push(pending);
    }
  }

  function handlePaste(e: ClipboardEvent) {
    const files = Array.from(e.clipboardData?.files ?? []).filter((f) =>
      f.type.startsWith("image/"),
    );
    if (files.length > 0) {
      e.preventDefault();
      void addFiles(files);
    }
  }

  function handleFilePicked(e: Event) {
    const input = e.currentTarget as HTMLInputElement;
    if (input.files) void addFiles(Array.from(input.files));
    input.value = "";
  }

  function removeImage(index: number) {
    const [removed] = images.splice(index, 1);
    if (removed && removed.previewUrl.startsWith("blob:")) {
      URL.revokeObjectURL(removed.previewUrl);
    }
  }

  function insertCommand(cmd: string) {
    inputVal = cmd;
    daemon.requestInputCompletions(cmd, cmd.length);
    if (textareaEl) {
      textareaEl.focus();
    }
  }

  function scalarToUtf16(text: string, scalarIndex: number): number {
    return Array.from(text).slice(0, scalarIndex).join("").length;
  }

  function acceptCompletion(item: InputCompletion) {
    const start = scalarToUtf16(inputVal, item.replace_start);
    const end = scalarToUtf16(inputVal, item.replace_end);
    inputVal = inputVal.slice(0, start) + item.insert_text + inputVal.slice(end);
    const caret = start + item.insert_text.length;
    resize();
    queueMicrotask(() => {
      textareaEl?.focus();
      textareaEl?.setSelectionRange(caret, caret);
      daemon.requestInputCompletions(inputVal, caret);
    });
  }
</script>

<footer class="composer-container">
  <div class="composer-box">
    {#if images.length > 0}
      <div class="image-chips">
        {#each images as img, i (img.previewUrl)}
          <span class="chip">
            <img src={img.previewUrl} alt="attachment" />
            <button class="remove" aria-label="Remove image" onclick={() => removeImage(i)}>×</button>
          </span>
        {/each}
      </div>
    {/if}

    <textarea
      bind:this={textareaEl}
      bind:value={inputVal}
      onkeydown={handleKeyDown}
      oninput={handleInput}
      onpaste={handlePaste}
      placeholder="Type your message, or ask coding tasks... (Enter to send, Shift+Enter for newline)"
      rows="1"
      disabled={!daemon.sessionAttached}
    ></textarea>

    {#if completionMatches.length > 0}
      <div class="command-completions" aria-label="Command completions">
        {#each completionMatches as item (`${item.kind}:${item.label}`)}
          <button type="button" onclick={() => acceptCompletion(item)}>
            <code>{item.label}</code>
            <span>{item.description}</span>
          </button>
        {/each}
      </div>
    {/if}

    <div class="toolbar">
      <div class="hints">
        <button class="hint-pill" onclick={() => insertCommand("/help")}>/help</button>
        <button class="hint-pill" onclick={() => insertCommand("/status")}>/status</button>
        <button class="hint-pill" onclick={() => insertCommand("/mcp")}>/mcp</button>
      </div>

      <div class="actions">
        <input
          bind:this={fileInputEl}
          type="file"
          accept="image/*"
          multiple
          class="file-input"
          onchange={handleFilePicked}
        />
        <button
          class="attach-btn"
          aria-label="Attach image"
          title="Attach image (or paste)"
          disabled={!daemon.sessionAttached}
          onclick={() => fileInputEl?.click()}
        >
          <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
            <path d="M21.44 11.05l-9.19 9.19a6 6 0 01-8.49-8.49l8.57-8.57A4 4 0 1118 8.84l-8.59 8.57a2 2 0 01-2.83-2.83l8.49-8.48"/>
          </svg>
        </button>
        <button
          class="send-btn"
          aria-label="Send message"
          disabled={(!inputVal.trim() && images.length === 0) || !daemon.sessionAttached}
          onclick={handleSend}
        >
          <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
            <path d="M22 2L11 13M22 2l-7 20-4-9-9-4 20-7z"/>
          </svg>
        </button>
      </div>
    </div>
  </div>
</footer>

<style>
  .composer-container {
    padding: 12px 24px 20px;
    background-color: var(--bg-app);
  }

  .composer-box {
    background-color: var(--input-bg-inactive);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-lg);
    padding: 12px 16px 8px;
    display: flex;
    flex-direction: column;
    transition: background-color 0.15s, border-color 0.15s;
  }

  /* The input component's two related-but-independent background tokens:
     inactive rests just above the page background; active (focus-within)
     lifts to the brighter input surface so the prompt is clearly the
     "typing lands here" target. */
  .composer-box:focus-within {
    background-color: var(--input-bg-active);
    border-color: var(--border-input-focus);
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

  .command-completions {
    display: grid;
    gap: 2px;
    margin: 6px -6px 2px;
    padding-top: 6px;
    border-top: 1px solid var(--border-subtle);
  }

  .command-completions button {
    display: grid;
    grid-template-columns: minmax(8rem, auto) 1fr;
    gap: 12px;
    align-items: baseline;
    padding: 6px;
    border: 0;
    border-radius: var(--radius-sm);
    color: var(--text-secondary);
    background: transparent;
    text-align: left;
    cursor: pointer;
  }

  .command-completions button:hover,
  .command-completions button:focus-visible {
    color: var(--text-primary);
    background: var(--bg-surface-hover);
    outline: none;
  }

  .command-completions code {
    color: var(--accent-primary);
  }

  .image-chips {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    margin-bottom: 8px;
  }

  .chip {
    position: relative;
    width: 48px;
    height: 48px;
    border-radius: var(--radius-md);
    overflow: hidden;
    border: 1px solid var(--border-strong);
  }

  .chip img {
    width: 100%;
    height: 100%;
    object-fit: cover;
    display: block;
  }

  .chip .remove {
    position: absolute;
    top: 2px;
    right: 2px;
    width: 16px;
    height: 16px;
    border-radius: 50%;
    border: none;
    background: rgba(0, 0, 0, 0.65);
    color: #fff;
    font-size: 11px;
    line-height: 1;
    cursor: pointer;
    display: flex;
    align-items: center;
    justify-content: center;
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

  .actions {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .file-input {
    display: none;
  }

  .attach-btn {
    width: 32px;
    height: 32px;
    border-radius: 50%;
    background: transparent;
    color: var(--text-muted);
    border: 1px solid var(--border-strong);
    display: flex;
    align-items: center;
    justify-content: center;
    cursor: pointer;
    transition: all 0.15s;
  }

  .attach-btn:not(:disabled):hover {
    color: var(--text-secondary);
    background: var(--bg-surface);
  }

  .attach-btn:disabled {
    opacity: 0.4;
    cursor: not-allowed;
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
