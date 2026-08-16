<script lang="ts">
  import { onMount } from "svelte";
  import { daemon } from "./lib/stores/daemon.svelte.js";
  import Sidebar from "./lib/components/Sidebar.svelte";
  import ChatHeader from "./lib/components/ChatHeader.svelte";
  import MessageItem from "./lib/components/MessageItem.svelte";
  import CommandBlock from "./lib/components/CommandBlock.svelte";
  import ToolCard from "./lib/components/ToolCard.svelte";
  import Composer from "./lib/components/Composer.svelte";
  import PermissionBanner from "./lib/components/PermissionBanner.svelte";
  import ToastStack from "./lib/components/ToastStack.svelte";
  import TodoPanel from "./lib/components/TodoPanel.svelte";
  import ModelPicker from "./lib/components/ModelPicker.svelte";
  import ConnectionDialog from "./lib/components/ConnectionDialog.svelte";

  let transcriptEl: HTMLElement;
  let autoScroll = $state(true);
  let sidebarOpen = $state(false);
  let modelsOpen = $state(false);
  let connectionOpen = $state(false);

  onMount(() => {
    // Surface unexpected client errors instead of dying silently.
    const onError = (event: Event) => {
      const message =
        event instanceof ErrorEvent
          ? event.message
          : (event as PromiseRejectionEvent).reason?.toString?.() ?? "unknown error";
      daemon.pushToast("error", "Client error", message);
    };
    window.addEventListener("error", onError);
    window.addEventListener("unhandledrejection", onError);

    daemon.init();

    // Open the connection dialog when there is nothing to talk to yet.
    const openTimer = window.setTimeout(() => {
      if (daemon.connection !== "connected") connectionOpen = true;
    }, 4000);

    return () => {
      window.removeEventListener("error", onError);
      window.removeEventListener("unhandledrejection", onError);
      window.clearTimeout(openTimer);
    };
  });

  $effect(() => {
    // Track transcript growth (feed length + streaming length) for
    // auto-scroll, cleaning up any in-flight scroll on re-run.
    void daemon.feed.length;
    void daemon.streamingAssistantText.length;
    void Object.keys(daemon.liveTools).length;
    const el = transcriptEl;
    if (!el || !autoScroll) return;
    const timer = window.setTimeout(() => {
      el.scrollTop = el.scrollHeight;
    }, 10);
    return () => window.clearTimeout(timer);
  });

  function handleScroll() {
    if (!transcriptEl) return;
    autoScroll =
      transcriptEl.scrollHeight - transcriptEl.scrollTop - transcriptEl.clientHeight < 80;
  }
</script>

<div class="layout">
  <Sidebar
    open={sidebarOpen}
    onClose={() => (sidebarOpen = false)}
    onOpenConnection={() => (connectionOpen = true)}
  />

  <main class="main">
    <ChatHeader
      onToggleSidebar={() => (sidebarOpen = !sidebarOpen)}
      onOpenModels={() => (modelsOpen = true)}
    />

    {#if daemon.reviewAlert}
      <div class="review-banner" role="alert">
        <pre>{daemon.reviewAlert}</pre>
        <button class="dismiss" aria-label="Dismiss review alert" onclick={() => daemon.dismissReviewAlert()}>
          ×
        </button>
      </div>
    {/if}

    <section class="transcript" bind:this={transcriptEl} onscroll={handleScroll}>
      {#if daemon.feed.length === 0 && !daemon.streamingAssistantText && Object.keys(daemon.liveTools).length === 0}
        <div class="empty-hero">
          <div class="icon">⚡</div>
          <h3>Neenee Session Workspace</h3>
          <p>
            {#if daemon.connection !== "connected"}
              Connecting to the session daemon…
            {:else if daemon.sessionAttached}
              Send prompts below to orchestrate coding turns.
            {:else}
              Select or create a session to start.
            {/if}
          </p>
          {#if daemon.sessionError}
            <p class="error-line">{daemon.sessionError}</p>
          {/if}
        </div>
      {:else}
        {#each daemon.feed as item (item.key)}
          {#if item.kind === "message"}
            <MessageItem message={item.message} />
          {:else}
            <CommandBlock record={item.record} />
          {/if}
        {/each}

        <!-- Active Tool Executions -->
        {#each Object.values(daemon.liveTools) as tool (tool.id)}
          <ToolCard {tool} />
        {/each}

        <!-- Streaming Assistant Text -->
        {#if daemon.streamingAssistantText || daemon.streamingReasoningText}
          <div class="message-bubble assistant streaming">
            {#if daemon.streamingReasoningText}
              <details class="reasoning">
                <summary>thinking…</summary>
                <pre>{daemon.streamingReasoningText}</pre>
              </details>
            {/if}
            {#if daemon.streamingAssistantText}
              <div class="stream-text">{daemon.streamingAssistantText}</div>
            {/if}
          </div>
        {/if}
      {/if}
    </section>

    <TodoPanel />

    <PermissionBanner />

    <Composer />
  </main>
</div>

{#if modelsOpen}
  <ModelPicker onclose={() => (modelsOpen = false)} />
{/if}

{#if connectionOpen}
  <ConnectionDialog onclose={() => (connectionOpen = false)} />
{/if}

<ToastStack />

<style>
  .layout {
    display: flex;
    height: 100vh;
    width: 100vw;
    background-color: var(--bg-app);
  }

  .main {
    flex: 1;
    display: flex;
    flex-direction: column;
    height: 100%;
    overflow: hidden;
    min-width: 0;
  }

  .review-banner {
    margin: 10px 24px 0;
    padding: 10px 12px;
    border: 1px solid rgba(210, 153, 34, 0.4);
    border-left: 3px solid var(--accent-warning);
    border-radius: var(--radius-md);
    background: rgba(210, 153, 34, 0.08);
    display: flex;
    align-items: flex-start;
    gap: 8px;
    flex-shrink: 0;
  }

  .review-banner pre {
    flex: 1;
    margin: 0;
    font-family: var(--font-mono);
    font-size: 11px;
    color: var(--text-secondary);
    white-space: pre-wrap;
    word-break: break-word;
    max-height: 120px;
    overflow-y: auto;
  }

  .review-banner .dismiss {
    background: transparent;
    border: none;
    color: var(--text-muted);
    font-size: 16px;
    cursor: pointer;
    line-height: 1;
  }

  .review-banner .dismiss:hover {
    color: var(--text-primary);
  }

  .transcript {
    flex: 1;
    overflow-y: auto;
    padding: 24px;
    display: flex;
    flex-direction: column;
  }

  .empty-hero {
    margin: auto;
    text-align: center;
    max-width: 440px;
  }

  .empty-hero .icon {
    font-size: 36px;
    margin-bottom: 12px;
  }

  .empty-hero h3 {
    font-size: 18px;
    font-weight: 600;
    margin-bottom: 6px;
  }

  .empty-hero p {
    font-size: 13px;
    color: var(--text-secondary);
  }

  .empty-hero .error-line {
    margin-top: 10px;
    color: var(--accent-danger);
    font-family: var(--font-mono);
    font-size: 12px;
    word-break: break-word;
  }

  .message-bubble.streaming {
    align-self: flex-start;
    width: 100%;
    margin-bottom: 16px;
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

  .stream-text {
    color: var(--text-primary);
    line-height: 1.6;
    font-size: 14px;
    white-space: pre-wrap;
    word-break: break-word;
  }

  @media (max-width: 900px) {
    .transcript {
      padding: 14px;
    }

    .review-banner {
      margin: 8px 14px 0;
    }
  }
</style>
