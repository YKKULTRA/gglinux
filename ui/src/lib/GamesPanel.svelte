<script lang="ts">
  // GameSense: the switch, where games find it, a test, and the games that have talked to it.

  import { daemon } from "./daemon.svelte";

  let gs = $derived(daemon.gamesense);
  let busy = $state(false);
  let error = $state<string | null>(null);

  $effect(() => {
    if (!daemon.connected) return;
    void daemon.getGamesenseState().catch((e: unknown) => {
      error = e instanceof Error ? e.message : String(e);
    });
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
</script>

<article class="panel">
  {#if gs}
    <section class="first">
      <label class="toggle">
        <input
          type="checkbox"
          checked={gs.enabled}
          disabled={busy}
          onchange={(e) => run(() => daemon.setGamesenseEnabled(e.currentTarget.checked))}
        />
        Let games control the keyboard lighting (GameSense)
      </label>
      <p class="note">
        Turning this on writes a small <code>coreProps.json</code> into each of Steam's Proton prefixes, so Windows
        games find gglinux; turning it off removes them. Games that support GameSense can take the keyboard over
        while they run, even with gglinux lighting off, and hand it back 15 seconds after they go quiet.
      </p>
      {#if gs.enabled}
        <p class="meta">Listening on <code>{gs.address}</code> · in {gs.prefixes} Proton prefix{gs.prefixes === 1 ? "" : "es"}</p>
      {/if}
      <button class="ghost" disabled={busy} onclick={() => run(() => daemon.gamesenseTest())}>
        Run test (F-key health bar, 20 s)
      </button>
    </section>

    <section>
      <div class="head"><h4>Games</h4></div>
      {#if gs.games.length === 0}
        <p class="meta">No game has talked to GameSense yet.</p>
      {:else}
        <ul class="games">
          {#each gs.games as g (g.name)}
            <li>
              <span class="dot" class:on={g.active} title={g.active ? "active" : "quiet"}></span>
              <div class="who">
                <strong>{g.display_name ?? g.name}</strong>
                {#if g.developer}<span class="meta">{g.developer}</span>{/if}
                <span class="meta">{g.events.join(", ")}</span>
              </div>
              <label class="toggle small">
                <input
                  type="checkbox"
                  checked={g.enabled}
                  disabled={busy}
                  onchange={(e) => run(() => daemon.setGameEnabled(g.name, e.currentTarget.checked))}
                />
                on
              </label>
              <button class="ghost" disabled={busy} onclick={() => run(() => daemon.removeGame(g.name))}>Remove</button>
            </li>
          {/each}
        </ul>
      {/if}
    </section>
  {/if}

  {#if error}
    <p class="error">{error}</p>
  {/if}
</article>

<style>
  .panel { padding: 1rem 1.1rem; background: var(--surface); border: 1px solid var(--line); border-radius: 12px; }
  section { margin-top: 1.1rem; padding-top: 1rem; border-top: 1px solid var(--line); }
  section.first { margin-top: 0; padding-top: 0; border-top: none; }
  h4 { margin: 0; font-size: 0.8rem; font-weight: 600; text-transform: uppercase; letter-spacing: 0.05em; color: var(--text-dim); }
  .head { display: flex; align-items: center; justify-content: space-between; margin-bottom: 0.6rem; }
  .toggle { display: flex; align-items: center; gap: 0.5rem; font-size: 0.85rem; cursor: pointer; }
  .toggle.small { font-size: 0.78rem; color: var(--text-dim); }
  .note { margin: 0.6rem 0 0; font-size: 0.8rem; color: var(--text-dim); max-width: 44rem; }
  .meta { margin: 0.5rem 0 0; font-size: 0.8rem; color: var(--text-dim); }
  code { font-size: 0.78rem; color: var(--text); }
  .ghost { margin-top: 0.75rem; padding: 0.3rem 0.65rem; font-size: 0.8rem; color: var(--text-dim); background: none; border: 1px solid var(--line); border-radius: 7px; cursor: pointer; }
  .ghost:hover:not(:disabled) { border-color: var(--accent); color: var(--accent); }
  .ghost:disabled { opacity: 0.4; cursor: default; }
  .games { list-style: none; margin: 0; padding: 0; }
  .games li { display: flex; align-items: center; gap: 0.75rem; padding: 0.55rem 0; border-top: 1px solid var(--line); }
  .games li:first-child { border-top: none; }
  .games li .ghost { margin-top: 0; }
  .who { flex: 1; display: flex; flex-direction: column; }
  .who .meta { margin: 0.1rem 0 0; }
  .dot { flex: none; width: 9px; height: 9px; border-radius: 50%; background: var(--line); }
  .dot.on { background: var(--accent); }
  .error { margin: 0.8rem 0 0; padding: 0.5rem 0.7rem; font-size: 0.8rem; color: var(--danger); background: color-mix(in srgb, var(--danger) 12%, transparent); border-radius: 8px; }
</style>
