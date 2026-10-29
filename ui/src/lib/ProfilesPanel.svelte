<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<script lang="ts">
  // Lighting profiles, and rules that switch to one while a game runs.

  import { daemon } from "./daemon.svelte";
  import type { GameRule } from "./types";

  let st = $derived(daemon.profiles);
  let busy = $state(false);
  let error = $state<string | null>(null);
  let newName = $state("");
  let newProcess = $state("");
  let newProfile = $state("");

  $effect(() => {
    if (!daemon.connected) return;
    void daemon.getProfiles().catch((e: unknown) => (error = e instanceof Error ? e.message : String(e)));
  });

  async function run(action: () => Promise<void>) {
    busy = true;
    error = null;
    try {
      await action();
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      busy = false;
    }
  }

  function setRules(rules: GameRule[]) {
    return run(() => daemon.setGameRules(rules));
  }
</script>

<article class="panel">
  {#if st}
    {#if st.active}
      <p class="now">Now: <strong>{st.active.profile}</strong>, because <code>{st.active.process}</code> is running</p>
    {/if}

    <section class="first">
      <div class="head"><h4>Profiles</h4></div>
      {#if st.profiles.length === 0}
        <p class="meta">No profiles yet. Set up the keyboard lighting, then save it here.</p>
      {/if}
      <ul class="rows">
        {#each st.profiles as name (name)}
          <li>
            <span class="grow">{name}</span>
            <button class="ghost" disabled={busy} onclick={() => run(() => daemon.applyProfile(name))}>Apply</button>
            <button class="ghost" disabled={busy} onclick={() => run(() => daemon.deleteProfile(name))}>Delete</button>
          </li>
        {/each}
      </ul>
      <div class="row">
        <input placeholder="Profile name" bind:value={newName} />
        <button class="ghost" disabled={busy || !newName.trim()} onclick={() => run(async () => { await daemon.saveProfile(newName); newName = ""; })}>
          Save current lighting
        </button>
      </div>
    </section>

    <section>
      <div class="head">
        <h4>Game rules</h4>
        <button class="ghost" disabled={busy} onclick={() => run(() => daemon.getProfiles().then(() => {}))}>Refresh running programs</button>
      </div>
      <ul class="rows">
        {#each st.rules as r, i (r.process + i)}
          <li>
            <code class="grow">{r.process}</code>
            <span>→ {r.profile}</span>
            <button class="ghost" disabled={busy} onclick={() => setRules(st!.rules.filter((_, j) => j !== i))}>Remove</button>
          </li>
        {/each}
      </ul>
      <div class="row">
        <input list="running" placeholder="Program (e.g. RDR2.exe)" bind:value={newProcess} />
        <datalist id="running">
          {#each st.running as p (p)}<option value={p}></option>{/each}
        </datalist>
        <select bind:value={newProfile}>
          <option value="" disabled>Profile</option>
          {#each st.profiles as name (name)}<option value={name}>{name}</option>{/each}
        </select>
        <button
          class="ghost"
          disabled={busy || !newProcess.trim() || !newProfile}
          onclick={() =>
            run(async () => {
              await daemon.setGameRules([...st!.rules, { process: newProcess, profile: newProfile }]);
              newProcess = "";
            })}
        >
          Add rule
        </button>
      </div>
      <p class="meta">While a listed program runs, its profile replaces the lighting; when it exits, the lighting goes back.</p>
    </section>
  {/if}
  {#if error}<p class="error">{error}</p>{/if}
</article>

<style>
  .panel { padding: 1rem 1.1rem; background: var(--surface); border: 1px solid var(--line); border-radius: 12px; }
  section { margin-top: 1.1rem; padding-top: 1rem; border-top: 1px solid var(--line); }
  section.first { margin-top: 0; padding-top: 0; border-top: none; }
  h4 { margin: 0; font-size: 0.8rem; font-weight: 600; text-transform: uppercase; letter-spacing: 0.05em; color: var(--text-dim); }
  .head { display: flex; align-items: center; justify-content: space-between; margin-bottom: 0.6rem; }
  .now { margin: 0 0 1rem; font-size: 0.85rem; }
  .rows { list-style: none; margin: 0; padding: 0; }
  .rows li { display: flex; align-items: center; gap: 0.6rem; padding: 0.4rem 0; border-top: 1px solid var(--line); font-size: 0.85rem; }
  .rows li:first-child { border-top: none; }
  .grow { flex: 1; }
  .row { display: flex; gap: 0.6rem; margin-top: 0.7rem; flex-wrap: wrap; }
  input, select { padding: 0.3rem 0.5rem; font-size: 0.82rem; color: var(--text); background: var(--surface-2); border: 1px solid var(--line); border-radius: 7px; }
  .meta { margin: 0.5rem 0 0; font-size: 0.8rem; color: var(--text-dim); }
  code { font-size: 0.8rem; }
  .ghost { padding: 0.25rem 0.55rem; font-size: 0.78rem; color: var(--text-dim); background: none; border: 1px solid var(--line); border-radius: 7px; cursor: pointer; }
  .ghost:hover:not(:disabled) { border-color: var(--accent); color: var(--accent); }
  .ghost:disabled { opacity: 0.4; cursor: default; }
  .error { margin: 0.8rem 0 0; padding: 0.5rem 0.7rem; font-size: 0.8rem; color: var(--danger); background: color-mix(in srgb, var(--danger) 12%, transparent); border-radius: 8px; }
</style>
