<script lang="ts">
  import { daemon } from "../stores/daemon.svelte.js";

  let inputText = $state("");
  let selected: number[][] = $state([]);
  // ADR-0141 web/TUI parity: free-text "Other" per question, mirroring the
  // TUI picker. An "other" answer replaces the option selection: the wire
  // carries the raw text as that question's single answer label.
  let otherText: string[] = $state([]);
  let otherActive: boolean[] = $state([]);
  let answerError = $state("");

  $effect(() => {
    // Reset the local answer state whenever a new question arrives.
    if (daemon.pendingQuestion) {
      const n = daemon.pendingQuestion.request.questions.length;
      selected = daemon.pendingQuestion.request.questions.map(() => []);
      otherText = Array.from({ length: n }, () => "");
      otherActive = Array.from({ length: n }, () => false);
      answerError = "";
    }
    if (daemon.pendingInput) {
      inputText = "";
    }
  });

  function toggleOption(qi: number, oi: number) {
    const q = daemon.pendingQuestion?.request.questions[qi];
    if (!q) return;
    const multi = q.multi_select;
    const current = selected[qi] ?? [];
    selected[qi] = multi
      ? current.includes(oi)
        ? current.filter((i) => i !== oi)
        : [...current, oi]
      : current.includes(oi)
        ? []
        : [oi];
    selected = [...selected];
  }

  function toggleOther(qi: number) {
    otherActive[qi] = !otherActive[qi];
    otherActive = [...otherActive];
    if (otherActive[qi]) selected[qi] = [];
    selected = [...selected];
  }

  /** A question is answered iff it has ≥1 selected option or non-blank Other. */
  function answered(qi: number): boolean {
    const picks = selected[qi] ?? [];
    if (otherActive[qi] && (otherText[qi] ?? "").trim() !== "") return true;
    return picks.length > 0;
  }

  function submitQuestion() {
    const req = daemon.pendingQuestion?.request;
    if (!req) return;
    // Zero-selection validation (ADR-0141): a non-empty outer array with
    // empty inner arrays is undefined in the parked-question protocol — the
    // reserved cancellation shape is an empty OUTER array. Refuse to send
    // an undefined payload; the operator must answer or explicitly cancel.
    const missing = req.questions
      .map((_, qi) => qi)
      .filter((qi) => !answered(qi));
    if (missing.length > 0) {
      answerError =
        missing.length === 1
          ? `Question ${missing[0] + 1} needs an answer — pick an option, write an Other answer, or cancel.`
          : `Questions ${missing.map((i) => i + 1).join(", ")} need answers — pick options, write Other answers, or cancel.`;
      return;
    }
    answerError = "";
    const answers = req.questions.map((_, qi) =>
      otherActive[qi] && (otherText[qi] ?? "").trim() !== ""
        ? [otherText[qi].trim()]
        : (selected[qi] ?? []).map((oi) => req.questions[qi].options[oi]?.label ?? ""),
    );
    daemon.answerQuestion(answers);
  }

  function cancelQuestion() {
    // The reserved cancellation shape: empty OUTER array. Settles the
    // parked question as cancelled; the model is told the operator declined
    // to answer.
    answerError = "";
    daemon.answerQuestion([]);
  }

  function submitInput() {
    daemon.replyInput(inputText);
    inputText = "";
  }
</script>

{#if daemon.pendingPermission}
  <div class="banner permission">
    <div class="head">
      <span class="icon" class:elevated={daemon.pendingPermission.request.elevation}>
        {daemon.pendingPermission.request.elevation ? "⚠" : "🔐"}
      </span>
      <span class="title">
        {daemon.pendingPermission.request.label || daemon.pendingPermission.request.tool}
      </span>
      {#if daemon.pendingPermission.origin.label}
        <span class="origin">runner: {daemon.pendingPermission.origin.label}</span>
      {/if}
    </div>
    {#if daemon.pendingPermission.request.description}
      <p class="desc">{daemon.pendingPermission.request.description}</p>
    {/if}
    <div class="args">
      <span class="label">scope: {daemon.pendingPermission.request.scope}</span>
      <pre>{daemon.pendingPermission.request.arguments}</pre>
    </div>
    <div class="actions">
      <button class="btn allow-once" onclick={() => daemon.resolvePermission("Once")}>
        Allow once
      </button>
      {#if !daemon.pendingPermission.request.one_off}
        <button class="btn allow-always" onclick={() => daemon.resolvePermission("Always")}>
          Always allow
        </button>
      {/if}
      <button class="btn deny" onclick={() => daemon.resolvePermission("Reject")}>
        Reject
      </button>
    </div>
  </div>
{:else if daemon.pendingQuestion}
  <div class="banner question">
    <div class="head">
      <span class="icon">❓</span>
      <span class="title">The agent needs your answer</span>
      {#if daemon.pendingQuestion.origin.label}
        <span class="origin">runner: {daemon.pendingQuestion.origin.label}</span>
      {/if}
    </div>
    {#each daemon.pendingQuestion.request.questions as q, qi (qi)}
      <div class="question-block">
        {#if q.header}<span class="q-header">{q.header}</span>{/if}
        <p class="q-text">{q.question}</p>
        <div class="options">
          {#each q.options as opt, oi (oi)}
            <button
              class="option"
              class:selected={(selected[qi] ?? []).includes(oi)}
              onclick={() => toggleOption(qi, oi)}
            >
              <span class="opt-label">{opt.label}</span>
              {#if opt.description}
                <span class="opt-desc">{opt.description}</span>
              {/if}
            </button>
          {/each}
        </div>
        <div class="other-row">
          <button
            class="option other-toggle"
            class:selected={otherActive[qi]}
            onclick={() => toggleOther(qi)}
          >Other…</button>
          {#if otherActive[qi]}
            <input
              class="other-input"
              type="text"
              bind:value={otherText[qi]}
              placeholder="Type a free-text answer"
              onkeydown={(e) => {
                if (e.key === "Enter") {
                  e.preventDefault();
                  submitQuestion();
                }
              }}
            />
          {/if}
        </div>
      </div>
    {/each}
    {#if answerError}<p class="answer-error">{answerError}</p>{/if}
    <div class="actions">
      <button class="btn allow-once" onclick={submitQuestion}>Answer</button>
      <button class="btn deny" onclick={cancelQuestion}>Cancel</button>
    </div>
  </div>
{:else if daemon.pendingInput}
  <div class="banner input">
    <div class="head">
      <span class="icon">⌨</span>
      <span class="title">{daemon.pendingInput.request.prompt}</span>
      {#if daemon.pendingInput.origin.label}
        <span class="origin">runner: {daemon.pendingInput.origin.label}</span>
      {/if}
    </div>
    <p class="desc mono">{daemon.pendingInput.request.command}</p>
    <div class="input-row">
      <input
        type={daemon.pendingInput.request.secret ? "password" : "text"}
        bind:value={inputText}
        placeholder={daemon.pendingInput.request.secret ? "secret input" : "input for the command"}
        onkeydown={(e) => {
          if (e.key === "Enter") {
            e.preventDefault();
            submitInput();
          }
        }}
      />
      <button class="btn allow-once" onclick={submitInput}>Send</button>
    </div>
  </div>
{/if}

<style>
  .other-row {
    display: flex;
    gap: 0.5rem;
    align-items: center;
    margin-top: 0.5rem;
  }
  .other-toggle {
    flex: 0 0 auto;
    font-style: italic;
  }
  .other-input {
    flex: 1;
  }
  .answer-error {
    color: var(--danger, #e5484d);
    margin: 0.5rem 0 0;
    font-size: 0.85rem;
  }
  .banner {
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-md);
    background-color: var(--bg-surface);
    padding: 12px 16px;
    margin: 8px 24px 16px;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }

  @media (max-width: 900px) {
    .banner {
      margin: 8px 14px 12px;
    }
  }

  .head {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .origin {
    font-family: var(--font-mono);
    font-size: 10px;
    padding: 1px 6px;
    border-radius: var(--radius-sm);
    background: rgba(210, 153, 34, 0.15);
    color: var(--accent-warning);
    text-transform: uppercase;
  }

  .icon {
    font-size: 14px;
  }

  .icon.elevated {
    color: var(--accent-warning);
  }

  .title {
    font-weight: 600;
    font-size: 14px;
    color: var(--text-primary);
  }

  .desc {
    font-size: 12px;
    color: var(--text-secondary);
    margin: 0;
  }

  .desc.mono {
    font-family: var(--font-mono);
  }

  .args {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }

  .label {
    font-family: var(--font-mono);
    font-size: 10px;
    color: var(--text-muted);
    text-transform: uppercase;
  }

  pre {
    background-color: var(--bg-surface-hover);
    border-radius: var(--radius-sm);
    padding: 8px;
    font-family: var(--font-mono);
    font-size: 11px;
    color: var(--text-secondary);
    white-space: pre-wrap;
    word-break: break-all;
    max-height: 140px;
    overflow-y: auto;
    margin: 0;
  }

  .actions {
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
  }

  .btn {
    font-size: 12px;
    font-weight: 500;
    padding: 6px 12px;
    border-radius: var(--radius-md);
    border: 1px solid var(--border-strong);
    background: transparent;
    cursor: pointer;
    transition: background-color 0.15s;
  }

  .allow-once {
    background-color: rgba(46, 160, 67, 0.15);
    border-color: rgba(46, 160, 67, 0.4);
    color: var(--accent-primary);
  }

  .allow-once:hover {
    background-color: rgba(46, 160, 67, 0.25);
  }

  .allow-always {
    background-color: rgba(88, 166, 255, 0.12);
    border-color: rgba(88, 166, 255, 0.4);
    color: var(--accent-info);
  }

  .allow-always:hover {
    background-color: rgba(88, 166, 255, 0.2);
  }

  .deny {
    background-color: rgba(248, 81, 73, 0.12);
    border-color: rgba(248, 81, 73, 0.4);
    color: var(--accent-danger);
  }

  .deny:hover {
    background-color: rgba(248, 81, 73, 0.22);
  }

  .question-block {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }

  .q-header {
    font-family: var(--font-mono);
    font-size: 10px;
    color: var(--text-muted);
    text-transform: uppercase;
  }

  .q-text {
    font-size: 13px;
    color: var(--text-primary);
    margin: 0;
  }

  .options {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }

  .option {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 2px;
    text-align: left;
    padding: 8px 10px;
    border-radius: var(--radius-md);
    border: 1px solid var(--border-subtle);
    background: transparent;
    cursor: pointer;
    transition: border-color 0.15s, background-color 0.15s;
  }

  .option:hover {
    background-color: var(--bg-surface-hover);
  }

  .option.selected {
    border-color: var(--accent-info);
    background-color: rgba(88, 166, 255, 0.1);
  }

  .opt-label {
    font-size: 13px;
    color: var(--text-primary);
    font-weight: 500;
  }

  .opt-desc {
    font-size: 11px;
    color: var(--text-muted);
  }

  .input-row {
    display: flex;
    gap: 8px;
  }

  .input-row input {
    flex: 1;
    background-color: var(--input-bg-inactive);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-md);
    padding: 8px 10px;
    color: var(--text-primary);
    font-size: 13px;
    font-family: var(--font-mono);
    outline: none;
  }

  .input-row input:focus {
    border-color: var(--border-input-focus);
  }
</style>
