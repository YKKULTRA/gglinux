<script lang="ts">
  // Sonar: Game / Chat / Media / Aux channels hosted by PipeWire, apps routed
  // into them by rule, and ChatMix between Game and Chat.

  import { daemon } from "./daemon.svelte";
  import type { Channel } from "./types";

  const CHANNELS: { value: Channel; label: string }[] = [
    { value: "game", label: "Game" },
    { value: "chat", label: "Chat" },
    { value: "media", label: "Media" },
    { value: "aux", label: "Aux" },
  ];
  const label = (c: Channel) => CHANNELS.find((x) => x.value === c)?.label ?? c;

  let sonar = $derived(daemon.sonar);
  let busy = $state<string | null>(null);
  let error = $state<string | null>(null);

  $effect(() => {
    if (!daemon.connected) return;
    void daemon.getSonarState().catch((e: unknown) => {
      error = e instanceof Error ? e.message : String(e);
    });
  });

  async function run(tag: string, op: () => Promise<void>) {
    busy = tag;
    error = null;
    try {
      await op();
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      busy = null;
    }
  }

  const pct = (v: number | null) => (v === null ? "–" : `${Math.round(v * 100)}%`);
</script>

<article class="panel">
  {#if !sonar}
    <p class="hint">Loading…</p>
  {:else if !sonar.available}
    <p class="hint">PipeWire isn't reachable, so Sonar can't run.</p>
  {:else}
    <header class="top">
      <div>
        <h3>Sonar</h3>
        <p class="meta">
          {sonar.enabled
            ? "Channels are live. Apps are routed by the rules below."
            : "Off. Your apps play straight to your default output."}
        </p>
      </div>
      <button
        class="apply"
        disabled={busy !== null}
        onclick={() => run("enable", () => daemon.setSonarEnabled(!sonar!.enabled))}
      >
        {busy === "enable" ? "Restarting PipeWire…" : sonar.enabled ? "Turn off" : "Turn on"}
      </button>
    </header>
    <p class="hint">Turning Sonar on or off restarts PipeWire: a second or two of silence.</p>

    <section>
      <div class="row">
        <span class="label">Output</span>
        <select
          value={sonar.output ?? ""}
          disabled={busy !== null}
          onchange={(e) => run("output", () => daemon.setSonarOutput(e.currentTarget.value))}
        >
          {#if !sonar.output}<option value="">chosen when Sonar turns on</option>{/if}
          {#each sonar.outputs as o (o.name)}<option value={o.name}>{o.description}</option>{/each}
        </select>
      </div>
    </section>

    {#if sonar.enabled}
      <section>
        <div class="head"><h4>Channels</h4></div>
        {#each sonar.channels as c (c.channel)}
          <div class="row">
            <span class="label">{label(c.channel)}</span>
            <input
              type="range"
              min="0"
              max="100"
              step="1"
              value={Math.round((c.volume ?? 1) * 100)}
              disabled={!c.present || busy !== null}
              onchange={(e) =>
                run("volume", () => daemon.setChannelVolume(c.channel, e.currentTarget.valueAsNumber / 100))}
            />
            <span class="unit">{c.present ? pct(c.volume) : "missing"}</span>
          </div>
        {/each}
      </section>

      <section>
        <div class="head">
          <h4>ChatMix</h4>
          <button class="ghost" disabled={busy !== null} onclick={() => run("chatmix", () => daemon.setChatMix(0))}>
            Centre
          </button>
        </div>
        <div class="row">
          <span class="label">Game</span>
          <input
            type="range"
            min="-100"
            max="100"
            step="5"
            value={Math.round(sonar.chatmix * 100)}
            disabled={busy !== null}
            onchange={(e) => run("chatmix", () => daemon.setChatMix(e.currentTarget.valueAsNumber / 100))}
          />
          <span class="unit">Chat</span>
        </div>
      </section>

      <section>
        <div class="head"><h4>Apps</h4></div>
        {#if sonar.apps.length === 0}
          <p class="hint">No app is playing audio right now.</p>
        {:else}
          <ul class="apps">
            {#each sonar.apps as app (app.id)}
              <li>
                <span class="app">{app.name}</span>
                <select
                  value={app.channel}
                  disabled={!app.binary || busy !== null}
                  onchange={(e) =>
                    run("route", () => daemon.setAppChannel(app.binary!, e.currentTarget.value as Channel))}
                >
                  {#each CHANNELS as c (c.value)}<option value={c.value}>{c.label}</option>{/each}
                </select>
              </li>
            {/each}
          </ul>
          <p class="hint">A change applies now and is remembered for that app.</p>
        {/if}
      </section>
    {/if}
  {/if}

  {#if error}
    <p class="error">{error}</p>
  {/if}
</article>

<style>
  .panel {
    padding: 1rem 1.1rem;
    background: var(--surface);
    border: 1px solid var(--line);
    border-radius: 12px;
  }
  .top {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 1rem;
  }
  h3 {
    margin: 0;
    font-size: 0.98rem;
    font-weight: 600;
  }
  h4 {
    margin: 0;
    font-size: 0.8rem;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--text-dim);
  }
  .meta {
    margin: 0.25rem 0 0;
    font-size: 0.82rem;
    color: var(--text-dim);
  }
  section {
    margin-top: 1.1rem;
    padding-top: 1rem;
    border-top: 1px solid var(--line);
  }
  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 0.6rem;
    margin-bottom: 0.6rem;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    margin-top: 0.5rem;
  }
  .label {
    width: 4.5rem;
    font-size: 0.82rem;
    color: var(--text-dim);
  }
  .row input[type="range"] {
    flex: 1;
    max-width: 18rem;
  }
  .unit {
    width: 3.5rem;
    font-size: 0.78rem;
    color: var(--text-dim);
  }
  select {
    padding: 0.3rem 0.45rem;
    font-size: 0.85rem;
    color: var(--text);
    background: var(--surface-2);
    border: 1px solid var(--line);
    border-radius: 7px;
  }
  .apps {
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .apps li {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 0.6rem;
    max-width: 24rem;
    padding: 0.2rem 0;
  }
  .app {
    font-size: 0.85rem;
  }
  .hint {
    margin: 0.6rem 0 0;
    font-size: 0.76rem;
    color: var(--text-dim);
  }
  .apply {
    padding: 0.4rem 0.85rem;
    font-size: 0.83rem;
    font-weight: 500;
    color: var(--text);
    background: var(--surface-2);
    border: 1px solid var(--line);
    border-radius: 8px;
    cursor: pointer;
  }
  .apply:hover:not(:disabled) {
    border-color: var(--accent);
    color: var(--accent);
  }
  .apply:disabled {
    opacity: 0.5;
    cursor: default;
  }
  .ghost {
    padding: 0.2rem 0.5rem;
    font-size: 0.76rem;
    color: var(--text-dim);
    background: none;
    border: 1px solid var(--line);
    border-radius: 7px;
    cursor: pointer;
  }
  .error {
    margin: 0.8rem 0 0;
    padding: 0.5rem 0.7rem;
    font-size: 0.8rem;
    color: var(--danger);
    background: color-mix(in srgb, var(--danger) 12%, transparent);
    border-radius: 8px;
  }
</style>
