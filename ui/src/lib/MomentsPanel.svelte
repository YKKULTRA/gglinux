<script lang="ts">
  // Instant replay: the recorder switch and settings, save, and the clip library.

  import { openFolder } from "./app";
  import { daemon } from "./daemon.svelte";
  import type { ClipQuality, ClipsSettings } from "./types";

  let st = $derived(daemon.clips);
  let busy = $state(false);
  let error = $state<string | null>(null);

  $effect(() => {
    if (!daemon.connected) return;
    void daemon.getClipsState().catch((e: unknown) => (error = e instanceof Error ? e.message : String(e)));
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

  // Each change sends the whole settings, so the controls wait while one is in flight:
  // a second change built from the old settings would undo the first.
  function set(patch: Partial<ClipsSettings>) {
    if (!st) return;
    void run(() => daemon.setClipsSettings({ ...st!.settings, ...patch }));
  }

  const size = (b: number) => (b > 1e9 ? `${(b / 1e9).toFixed(1)} GB` : `${(b / 1e6).toFixed(1)} MB`);
  const when = (s: number) => new Date(s * 1000).toLocaleString();
  const QUALITIES: ClipQuality[] = ["medium", "high", "very_high", "ultra"];
  // The usual rates, plus whatever clips.json holds (anything from 15 to 240 is allowed).
  let rates = $derived(st ? [...new Set([30, 60, 120, 144, st.settings.fps])].sort((a, b) => a - b) : []);
</script>

<article class="panel">
  {#if st}
    {#if !st.available}
      <p class="notice">
        Clips need <code>gpu-screen-recorder</code>. Install it with
        <code>sudo pacman -S gpu-screen-recorder</code>, then come back.
      </p>
    {/if}
    <section class="first">
      <label class="toggle">
        <!-- Turning off stays possible if the recorder was uninstalled while on. -->
        <input
          type="checkbox"
          checked={st.enabled}
          disabled={busy || (!st.available && !st.enabled)}
          onchange={(e) => set({ enabled: e.currentTarget.checked })}
        />
        Keep an instant-replay buffer
      </label>
      <label class="toggle small">
        <input
          type="checkbox"
          checked={st.settings.games_only}
          disabled={busy}
          onchange={(e) => set({ games_only: e.currentTarget.checked })}
        />
        Only while a game from your game rules runs
      </label>
      <p class="meta">
        While on, the last {st.settings.seconds} seconds of the screen are kept in memory and nothing is written until you save.
        {st.recording ? (st.settings.games_only && st.game ? `Recording while ${st.game} runs.` : "Recording now.") : ""}
      </p>
      {#if st.waiting}
        <p class="meta">Waiting for a game from your game rules (Settings → Profiles) to start.</p>
      {/if}
      <div class="row">
        <span class="label">Length</span>
        <input type="range" min="5" max="300" step="5" value={st.settings.seconds} disabled={busy} onchange={(e) => set({ seconds: e.currentTarget.valueAsNumber })} />
        <span class="unit">{st.settings.seconds} s</span>
      </div>
      <div class="row">
        <span class="label">Frame rate</span>
        <select value={st.settings.fps} disabled={busy} onchange={(e) => set({ fps: Number(e.currentTarget.value) })}>
          {#each rates as f (f)}<option value={f}>{f} fps</option>{/each}
        </select>
        <span class="label">Quality</span>
        <select value={st.settings.quality} disabled={busy} onchange={(e) => set({ quality: e.currentTarget.value as ClipQuality })}>
          {#each QUALITIES as q (q)}<option value={q}>{q.replace("_", " ")}</option>{/each}
        </select>
      </div>
      <div class="row">
        <label class="toggle small"><input type="checkbox" checked={st.settings.desktop_audio} disabled={busy} onchange={(e) => set({ desktop_audio: e.currentTarget.checked })} /> Desktop audio</label>
        <label class="toggle small"><input type="checkbox" checked={st.settings.mic} disabled={busy} onchange={(e) => set({ mic: e.currentTarget.checked })} /> Microphone</label>
      </div>
      <div class="row">
        <button class="primary" disabled={busy || !st.recording} onclick={() => run(() => daemon.saveClip())}>Save clip</button>
        {#if st.hotkey}
          <span class="meta">Hotkey: {st.hotkey}</span>
        {:else if st.hotkey_error}
          <span class="meta warn">{st.hotkey_error}</span>
        {:else}
          <span class="meta">No hotkey yet: KDE asks to confirm Alt+S when clips start.</span>
        {/if}
      </div>
      {#if st.error}<p class="meta warn">{st.error}</p>{/if}
    </section>

    <section>
      <div class="head">
        <h4>Clips</h4>
        <button class="ghost" onclick={() => run(() => openFolder(st!.dir))}>Open folder</button>
      </div>
      {#if st.clips.length === 0}
        <p class="meta">No clips yet in <code>{st.dir}</code>.</p>
      {:else}
        <ul class="rows">
          {#each st.clips as c (c.name)}
            <li>
              <span class="grow">{c.name}</span>
              <span class="meta">{size(c.bytes)} · {when(c.modified)}</span>
              <button class="ghost" disabled={busy} onclick={() => run(() => daemon.deleteClip(c.name))}>Delete</button>
            </li>
          {/each}
        </ul>
      {/if}
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
  .notice { margin: 0 0 1rem; padding: 0.55rem 0.75rem; font-size: 0.82rem; background: var(--surface-2); border: 1px solid var(--line); border-radius: 8px; }
  .toggle { display: flex; align-items: center; gap: 0.5rem; font-size: 0.85rem; cursor: pointer; }
  .toggle.small { font-size: 0.8rem; color: var(--text-dim); }
  .row { display: flex; align-items: center; gap: 0.6rem; margin-top: 0.7rem; flex-wrap: wrap; }
  .label { width: 5.5rem; font-size: 0.82rem; color: var(--text-dim); }
  .unit, .meta { font-size: 0.8rem; color: var(--text-dim); }
  .meta { margin: 0.4rem 0 0; }
  .warn { color: var(--danger); }
  select { padding: 0.25rem 0.4rem; font-size: 0.82rem; color: var(--text); background: var(--surface-2); border: 1px solid var(--line); border-radius: 7px; }
  code { font-size: 0.78rem; }
  .rows { list-style: none; margin: 0; padding: 0; }
  .rows li { display: flex; align-items: center; gap: 0.6rem; padding: 0.4rem 0; border-top: 1px solid var(--line); font-size: 0.85rem; }
  .rows li:first-child { border-top: none; }
  .grow { flex: 1; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .primary { padding: 0.4rem 0.9rem; font-size: 0.85rem; font-weight: 600; color: var(--bg); background: var(--accent); border: none; border-radius: 8px; cursor: pointer; }
  .primary:disabled { opacity: 0.4; cursor: default; }
  .ghost { padding: 0.25rem 0.55rem; font-size: 0.78rem; color: var(--text-dim); background: none; border: 1px solid var(--line); border-radius: 7px; cursor: pointer; }
  .ghost:hover:not(:disabled) { border-color: var(--accent); color: var(--accent); }
  .ghost:disabled { opacity: 0.4; cursor: default; }
  .error { margin: 0.8rem 0 0; padding: 0.5rem 0.7rem; font-size: 0.8rem; color: var(--danger); background: color-mix(in srgb, var(--danger) 12%, transparent); border-radius: 8px; }
</style>
