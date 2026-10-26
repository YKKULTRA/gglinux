<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<script lang="ts">
  // Start at login, and backups of every setting. The UI process does these
  // itself, so they work while the daemon is down.

  import { exportSettings, getAutostart, importSettings, openFolder, setAutostart } from "./app";

  let autostart = $state<boolean | null>(null);
  let busy = $state(false);
  let note = $state<string | null>(null);
  let error = $state<string | null>(null);
  let lastExport = $state<string | null>(null);
  let picker = $state<HTMLInputElement>();

  $effect(() => {
    void getAutostart()
      .then((on) => (autostart = on))
      .catch((e: unknown) => (error = e instanceof Error ? e.message : String(e)));
  });

  async function run(action: () => Promise<void>) {
    busy = true;
    error = null;
    note = null;
    try {
      await action();
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      busy = false;
    }
  }

  // Read it back whatever happened: the entry is written even when the daemon's
  // service step fails, and a change that was refused has to untick the box.
  function toggleAutostart(box: HTMLInputElement) {
    void run(async () => {
      try {
        await setAutostart(box.checked);
      } finally {
        autostart = await getAutostart();
        box.checked = autostart;
      }
    });
  }

  function pick(e: Event & { currentTarget: HTMLInputElement }) {
    const file = e.currentTarget.files?.[0];
    // Cleared, so picking the same file again still fires a change.
    e.currentTarget.value = "";
    if (!file) return;
    void run(async () => {
      note = await importSettings(await file.text());
    });
  }

  const folderOf = (p: string) => p.slice(0, p.lastIndexOf("/")) || "/";
</script>

<article class="panel">
  <section class="first">
    <label class="toggle">
      <input
        type="checkbox"
        checked={autostart ?? false}
        disabled={busy || autostart === null}
        onchange={(e) => toggleAutostart(e.currentTarget)}
      />
      Start gglinux when I log in (the daemon, and this window hidden in the tray)
    </label>
  </section>

  <section>
    <div class="head"><h4>Backup</h4></div>
    <div class="row">
      <button class="ghost" disabled={busy} onclick={() => run(async () => { lastExport = await exportSettings(); })}>Back up settings</button>
      <button class="ghost" disabled={busy} onclick={() => picker?.click()}>Restore from a backup…</button>
      <input bind:this={picker} type="file" accept=".json,application/json" hidden onchange={pick} />
    </div>
    {#if lastExport}
      <p class="meta">
        Saved to <code>{lastExport}</code>
        <button class="link" onclick={() => run(() => openFolder(folderOf(lastExport!)))}>Open folder</button>
      </p>
    {/if}
    {#if note}<p class="meta">{note}</p>{/if}
  </section>
  {#if error}<p class="error">{error}</p>{/if}
</article>

<style>
  .panel { margin-top: 1rem; padding: 1rem 1.1rem; background: var(--surface); border: 1px solid var(--line); border-radius: 12px; }
  section { margin-top: 1.1rem; padding-top: 1rem; border-top: 1px solid var(--line); }
  section.first { margin-top: 0; padding-top: 0; border-top: none; }
  h4 { margin: 0; font-size: 0.8rem; font-weight: 600; text-transform: uppercase; letter-spacing: 0.05em; color: var(--text-dim); }
  .head { margin-bottom: 0.6rem; }
  .toggle { display: flex; align-items: center; gap: 0.5rem; font-size: 0.85rem; cursor: pointer; }
  .row { display: flex; gap: 0.6rem; flex-wrap: wrap; }
  .meta { margin: 0.6rem 0 0; font-size: 0.8rem; color: var(--text-dim); }
  code { font-size: 0.78rem; }
  .ghost { padding: 0.25rem 0.55rem; font-size: 0.78rem; color: var(--text-dim); background: none; border: 1px solid var(--line); border-radius: 7px; cursor: pointer; }
  .ghost:hover:not(:disabled) { border-color: var(--accent); color: var(--accent); }
  .ghost:disabled { opacity: 0.4; cursor: default; }
  .link { padding: 0; margin-left: 0.3rem; font-size: 0.8rem; color: var(--accent); background: none; border: none; cursor: pointer; }
  .error { margin: 0.8rem 0 0; padding: 0.5rem 0.7rem; font-size: 0.8rem; color: var(--danger); background: color-mix(in srgb, var(--danger) 12%, transparent); border-radius: 8px; }
</style>
