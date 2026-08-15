<script lang="ts">
  import { onMount } from "svelte";
  import { daemon } from "./lib/stores/daemon.svelte.js";
  import Sidebar from "./lib/components/Sidebar.svelte";
  import ChatHeader from "./lib/components/ChatHeader.svelte";
  import MessageItem from "./lib/components/MessageItem.svelte";
  import ToolCard from "./lib/components/ToolCard.svelte";
  import Composer from "./lib/components/Composer.svelte";

  let transcriptEl: HTMLElement;

  onMount(() => {
    daemon.init();
  });

  $effect(() => {
    // Auto-scroll on new messages or deltas
    if (daemon.messages.length || daemon.streamingAssistantText || Object.keys(daemon.liveTools).length) {
      if (transcriptEl) {
        setTimeout(() => {
          transcriptEl.scrollTop = transcriptEl.scrollHeight;
        }, 10);
      }
    }
  });
</script>

<div class="layout">
  <Sidebar />

  <main class="main">
    <ChatHeader />

    <section class="transcript" bind:this={transcriptEl}>
      {#if daemon.messages.length === 0 && !daemon.streamingAssistantText && Object.keys(daemon.liveTools).length === 0}
        <div class="empty-hero">
          <div class="icon">⚡</div>
          <h3>Neenee Session Workspace</h3>
          <p>Connected to the session daemon. Send prompts below to orchestrate coding turns.</p>
        </div>
      {:else}
        {#each daemon.messages as msg, i (i)}
          <MessageItem message={msg} />
        {/each}

        <!-- Active Tool Executions -->
        {#each Object.values(daemon.liveTools) as tool (tool.id)}
          <ToolCard {tool} />
        {/each}

        <!-- Streaming Assistant Text -->
        {#if daemon.streamingAssistantText}
          <MessageItem
            message={{
              role: "assistant",
              content: daemon.streamingAssistantText,
              timestamp: Date.now()
            }}
          />
        {/if}
      {/if}
    </section>

    <Composer />
  </main>
</div>

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
</style>
