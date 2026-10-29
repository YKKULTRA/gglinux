<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<script lang="ts">
  // Which parts of gglinux run. One that's off isn't started: its device is left alone,
  // nothing of it runs in the background, and its page is hidden. Its settings stay.
  // Applying restarts the daemon, which the bridge reconnects to by itself.

  import { daemon } from "./daemon.svelte";
  import type { Part, Parts } from "./types";
  import { ALL_PARTS } from "./types";

  const PARTS: { id: Part; label: string; what: string; off?: string }[] = [
    { id: "keyboard", label: "Keyboard", what: "Apex Pro: lighting, screen and switches." },
    {
      id: "headset",
      label: "Headset",
      what: "Arctis Nova Pro: battery, controls, and keeping the base station's screen dark.",
      off: "The base station's screen won't be kept dark any more, so it can burn in.",
    },
    { id: "mouse", label: "Mouse", what: "Rival 3: DPI, polling rate, buttons and battery." },
    {
      id: "sonar",
      label: "Sonar",
      what: "Audio channels, EQ, the mic, Smart Volume and streamer mode.",
      off: "If Sonar's channels are on, they're taken out of PipeWire (it restarts once).",
    },
    { id: "gamesense", label: "GameSense", what: "Games drive the keyboard lighting." },
    {
      id: "profiles",
      label: "Profiles",
      what: "Saved lighting, switched by the game you're playing.",
      off: "Games-only clips find the game through Profiles' rules, so they won't start.",
    },
    { id: "clips", label: "Clips", what: "Instant replay and the Moments page." },
  ];

  let draft = $state<Parts>({ ...ALL_PARTS });
  let busy = $state(false);
  let error = $state<string | null>(null);

  // Start from what the daemon runs, and again after each restart.
  $effect(() => {
    if (daemon.parts) draft = { ...daemon.parts };
  });

  let saved = $derived(daemon.parts ?? ALL_PARTS);
  let changed = $derived(PARTS.some((p) => draft[p.id] !== saved[p.id]));

  function note(id: Part): string | null {
    if (id === "gamesense" && draft.gamesense && !draft.keyboard) {
      return "GameSense draws on the keyboard, so it waits until Keyboard is on.";
    }
    const p = PARTS.find((q) => q.id === id);
    return !draft[id] && saved[id] && p?.off ? p.off : null;
  }

  async function apply() {
    busy = true;
    error = null;
    try {
      await daemon.setParts({ ...draft });
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      busy = false;
    }
  }
</script>

<article class="panel">
  <div class="head">
    <h3>Parts of gglinux</h3>
    <p>
      Turn off what you don't use. A part that's off doesn't run at all: its device is left alone
      and its page is hidden. Its settings are kept for when you turn it back on.
    </p>
  </div>
  <ul>
    {#each PARTS as p (p.id)}
      <li>
        <label class="toggle">
          <input type="checkbox" bind:checked={draft[p.id]} disabled={busy || daemon.restarting} />
          <span class="name">{p.label}</span>
          <span class="what">{p.what}</span>
        </label>
        {#if note(p.id)}<p class="note">{note(p.id)}</p>{/if}
      </li>
    {/each}
  </ul>
  <div class="row">
    <button class="apply" disabled={busy || daemon.restarting || !changed} onclick={apply}>Apply</button>
    {#if daemon.restarting}
      <span class="meta">Starting gglinuxd again with the new parts…</span>
    {:else if changed}
      <span class="meta">Applying restarts gglinuxd, which takes a few seconds.</span>
    {/if}
  </div>
  {#if error}<p class="error">{error}</p>{/if}
</article>

<style>
  .panel { margin-bottom: 1rem; padding: 1rem 1.1rem; background: var(--surface); border: 1px solid var(--line); border-radius: 12px; }
  .head h3 { margin: 0; font-size: 0.95rem; font-weight: 600; }
  .head p { margin: 0.3rem 0 0; font-size: 0.8rem; color: var(--text-dim); line-height: 1.45; }
  ul { list-style: none; margin: 0.8rem 0 0; padding: 0; display: flex; flex-direction: column; gap: 0.45rem; }
  .toggle { display: grid; grid-template-columns: auto 6.5rem 1fr; align-items: baseline; gap: 0.55rem; font-size: 0.85rem; cursor: pointer; }
  .name { font-weight: 550; }
  .what { font-size: 0.8rem; color: var(--text-dim); }
  .note { margin: 0.25rem 0 0 1.6rem; font-size: 0.78rem; color: var(--warn, var(--danger)); }
  .row { display: flex; align-items: center; gap: 0.8rem; margin-top: 0.9rem; flex-wrap: wrap; }
  .meta { font-size: 0.8rem; color: var(--text-dim); }
  .apply { padding: 0.4rem 0.85rem; font-size: 0.83rem; font-weight: 500; color: var(--text); background: var(--surface-2); border: 1px solid var(--line); border-radius: 8px; cursor: pointer; }
  .apply:hover:not(:disabled) { border-color: var(--accent); color: var(--accent); }
  .apply:disabled { opacity: 0.5; cursor: default; }
  .error { margin: 0.8rem 0 0; padding: 0.5rem 0.7rem; font-size: 0.8rem; color: var(--danger); background: color-mix(in srgb, var(--danger) 12%, transparent); border-radius: 8px; }
</style>
