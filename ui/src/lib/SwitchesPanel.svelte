<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<script lang="ts">
  // The Apex's adjustable switches: actuation point, Rapid Trigger and Rapid Tap. The
  // keyboard keeps them in RAM, not flash, so the daemon sends them again whenever it
  // connects; turned off, the keyboard keeps the last ones until it loses power.

  import { SvelteSet } from "svelte/reactivity";
  import { daemon } from "./daemon.svelte";
  import type { KeyboardState, Switches, TapMode, TapPair } from "./types";

  let { kb }: { kb: KeyboardState } = $props();

  /** Pixels per key unit in the picture. */
  const U = 40;
  const MAX_PAIRS = 10;
  const TAP_MODES: { mode: TapMode; label: string }[] = [
    { mode: "last_wins", label: "Last pressed wins" },
    { mode: "neither", label: "Neither counts" },
    { mode: "first_wins", label: "First key wins" },
    { mode: "second_wins", label: "Second key wins" },
  ];

  let sw = $derived(kb.switches);
  let analog = $derived(new Set(kb.analog_keys));
  let analogKeys = $derived(kb.layout.filter((k) => analog.has(k.code)));
  let own = $derived(new Map(sw.keys.map((k) => [k.code, k.actuation])));
  let busy = $state(false);
  let error = $state<string | null>(null);
  const selected = new SvelteSet<number>();
  let ownLevel = $state(15);

  let width = $derived(Math.max(...kb.layout.map((k) => k.x + k.w)) * U);
  let height = $derived(Math.max(...kb.layout.map((k) => k.y + 1)) * U);

  const mm = (tenths: number) => (tenths / 10).toFixed(1);
  const rapidOn = (code: number) =>
    sw.rapid_trigger.enabled && (sw.rapid_trigger.keys.length === 0 || sw.rapid_trigger.keys.includes(code));

  /** Each change sends all the switch settings, so the controls wait while one is in
   *  flight: a second change built from the old settings would undo the first. */
  async function apply(patch: Partial<Switches>) {
    busy = true;
    error = null;
    try {
      await daemon.setSwitches({ ...sw, ...patch });
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      busy = false;
    }
  }

  function toggle(code: number) {
    if (!analog.has(code)) return;
    if (selected.has(code)) selected.delete(code);
    else selected.add(code);
  }

  /** Give the selected keys their own actuation point, or (`null`) the default again. */
  function setOwn(level: number | null) {
    const keys = sw.keys.filter((k) => !selected.has(k.code));
    if (level !== null) for (const code of selected) keys.push({ code, actuation: level });
    void apply({ keys });
  }

  function setRapid(patch: Partial<Switches["rapid_trigger"]>) {
    void apply({ rapid_trigger: { ...sw.rapid_trigger, ...patch } });
  }

  function setTap(patch: Partial<Switches["rapid_tap"]>) {
    void apply({ rapid_tap: { ...sw.rapid_tap, ...patch } });
  }

  function setPair(i: number, patch: Partial<TapPair>) {
    setTap({ pairs: sw.rapid_tap.pairs.map((p, j) => (j === i ? { ...p, ...patch } : p)) });
  }

  /** A and D first (counter-strafing), then the first two keys in no pair. */
  function addPair() {
    const used = new Set(sw.rapid_tap.pairs.flatMap((p) => [p.a, p.b]));
    const free = kb.analog_keys.filter((c) => !used.has(c));
    const [a, b] = !used.has(0x04) && !used.has(0x07) ? [0x04, 0x07] : [free[0], free[1]];
    if (a === undefined || b === undefined) return;
    setTap({ pairs: [...sw.rapid_tap.pairs, { a, b, mode: "last_wins", report_both: false }] });
  }

  function removePair(i: number) {
    setTap({ pairs: sw.rapid_tap.pairs.filter((_, j) => j !== i) });
  }
</script>

<section>
  <div class="head"><h4>Switches</h4></div>
  <label class="toggle">
    <input
      type="checkbox"
      checked={sw.enabled}
      disabled={busy}
      onchange={(e) => apply({ enabled: e.currentTarget.checked })}
    />
    gglinux sets the switches (off: the keyboard keeps its own)
  </label>
  <p class="hint">
    The keyboard keeps these in memory, not in its flash, so gglinux sends them again each time it
    connects. Turned off, the keyboard keeps the last ones until it's unplugged.
  </p>

  {#if sw.enabled}
    <div class="row">
      <span class="label">Actuation</span>
      <input
        type="range"
        min="1"
        max="40"
        step="1"
        value={sw.actuation}
        disabled={busy}
        onchange={(e) => apply({ actuation: e.currentTarget.valueAsNumber })}
      />
      <span class="unit">{mm(sw.actuation)} mm for every key without its own</span>
    </div>

    <svg class="board" viewBox="-4 -4 {width + 8} {height + 8}" role="group" aria-label="Keyboard switches">
      {#each kb.layout as k (k.code)}
        <g
          class="key"
          class:selected={selected.has(k.code)}
          class:fixed={!analog.has(k.code)}
          class:own={own.has(k.code)}
          class:rapid={analog.has(k.code) && rapidOn(k.code)}
        >
          <rect
            x={k.x * U + 2}
            y={k.y * U + 2}
            width={k.w * U - 4}
            height={U - 4}
            rx="5"
            role="button"
            tabindex="-1"
            aria-label={k.label || "Space"}
            onclick={() => toggle(k.code)}
            onkeydown={() => {}}
          />
          <text x={k.x * U + (k.w * U) / 2} y={k.y * U + U / 2 - 1}>{k.label}</text>
          {#if analog.has(k.code)}
            <text class="mm" x={k.x * U + (k.w * U) / 2} y={k.y * U + U / 2 + 11}>
              {mm(own.get(k.code) ?? sw.actuation)}
            </text>
          {/if}
        </g>
      {/each}
    </svg>
    <p class="hint">
      Click keys to select them. Grey keys have fixed switches; keys with their own point are outlined,
      and Rapid Trigger keys are tinted.
    </p>
    <div class="row">
      <span class="label">Selected ({selected.size})</span>
      <input type="range" min="1" max="40" step="1" bind:value={ownLevel} />
      <span class="unit">{mm(ownLevel)} mm</span>
      <button class="ghost" disabled={busy || selected.size === 0} onclick={() => setOwn(ownLevel)}>
        Set for selected
      </button>
      <button class="ghost" disabled={busy || selected.size === 0} onclick={() => setOwn(null)}>
        Back to the default
      </button>
      <button class="ghost" disabled={selected.size === 0} onclick={() => selected.clear()}>Clear selection</button>
    </div>

    <h5>Rapid Trigger</h5>
    <label class="toggle">
      <input
        type="checkbox"
        checked={sw.rapid_trigger.enabled}
        disabled={busy}
        onchange={(e) => setRapid({ enabled: e.currentTarget.checked })}
      />
      A key resets as soon as it starts to rise, and fires again as soon as it goes back down
    </label>
    {#if sw.rapid_trigger.enabled}
      <div class="row">
        <span class="label">Distance</span>
        <input
          type="range"
          min="1"
          max="20"
          step="1"
          value={sw.rapid_trigger.sensitivity}
          disabled={busy}
          onchange={(e) => setRapid({ sensitivity: e.currentTarget.valueAsNumber })}
        />
        <span class="unit">{mm(sw.rapid_trigger.sensitivity)} mm</span>
      </div>
      <div class="row">
        <span class="label">Keys</span>
        <span class="unit">
          {sw.rapid_trigger.keys.length === 0 ? "every adjustable key" : `${sw.rapid_trigger.keys.length} keys`}
        </span>
        <button class="ghost" disabled={busy || selected.size === 0} onclick={() => setRapid({ keys: [...selected] })}>
          Only the selected
        </button>
        <button class="ghost" disabled={busy || sw.rapid_trigger.keys.length === 0} onclick={() => setRapid({ keys: [] })}>
          Every key
        </button>
      </div>
    {/if}

    <h5>Rapid Tap</h5>
    <label class="toggle">
      <input
        type="checkbox"
        checked={sw.rapid_tap.enabled}
        disabled={busy}
        onchange={(e) => setTap({ enabled: e.currentTarget.checked })}
      />
      With both keys of a pair held, the keyboard decides which one counts (SOCD)
    </label>
    {#if sw.rapid_tap.enabled}
      {#each sw.rapid_tap.pairs as p, i (i)}
        <div class="row">
          <select value={p.a} disabled={busy} onchange={(e) => setPair(i, { a: Number(e.currentTarget.value) })}>
            {#each analogKeys as k (k.code)}<option value={k.code}>{k.label || "Space"}</option>{/each}
          </select>
          <span class="unit">and</span>
          <select value={p.b} disabled={busy} onchange={(e) => setPair(i, { b: Number(e.currentTarget.value) })}>
            {#each analogKeys as k (k.code)}<option value={k.code}>{k.label || "Space"}</option>{/each}
          </select>
          <select
            value={p.mode}
            disabled={busy}
            onchange={(e) => setPair(i, { mode: e.currentTarget.value as TapMode })}
          >
            {#each TAP_MODES as m (m.mode)}<option value={m.mode}>{m.label}</option>{/each}
          </select>
          <label class="toggle">
            <input
              type="checkbox"
              checked={p.report_both}
              disabled={busy}
              onchange={(e) => setPair(i, { report_both: e.currentTarget.checked })}
            />
            send both
          </label>
          <button class="ghost" disabled={busy} onclick={() => removePair(i)}>Remove</button>
        </div>
      {/each}
      <div class="row">
        <button class="ghost" disabled={busy || sw.rapid_tap.pairs.length >= MAX_PAIRS} onclick={addPair}>Add a pair</button>
        <span class="unit">{sw.rapid_tap.pairs.length} of {MAX_PAIRS}</span>
      </div>
      <p class="hint">What each mode does comes from other projects' notes; it hasn't been checked on this keyboard yet.</p>
    {/if}
  {/if}

  {#if kb.switches_error}
    <p class="error">The keyboard refused the switch settings: {kb.switches_error}</p>
  {/if}
  {#if error}
    <p class="error">{error}</p>
  {/if}
</section>

<style>
  section { margin-top: 1.1rem; padding-top: 1rem; border-top: 1px solid var(--line); }
  .head { display: flex; align-items: center; justify-content: space-between; gap: 0.6rem; margin-bottom: 0.6rem; }
  h4 { margin: 0; font-size: 0.8rem; font-weight: 600; text-transform: uppercase; letter-spacing: 0.05em; color: var(--text-dim); }
  h5 { margin: 1rem 0 0.5rem; font-size: 0.8rem; font-weight: 600; color: var(--text); }
  .toggle { display: flex; align-items: center; gap: 0.5rem; font-size: 0.85rem; cursor: pointer; }
  .row { display: flex; align-items: center; gap: 0.6rem; margin-top: 0.7rem; flex-wrap: wrap; }
  .label { width: 5.5rem; font-size: 0.82rem; color: var(--text-dim); }
  .unit { font-size: 0.78rem; color: var(--text-dim); }
  .hint { margin: 0.5rem 0 0; font-size: 0.78rem; color: var(--text-dim); }
  .board { width: 100%; max-width: 48rem; height: auto; margin-top: 0.8rem; user-select: none; }
  .key rect { fill: var(--surface-2); stroke: var(--line); stroke-width: 1.5; cursor: pointer; }
  .key.fixed rect { opacity: 0.35; cursor: default; }
  .key.own rect { stroke: var(--text); }
  .key.rapid rect { fill: color-mix(in srgb, var(--accent) 22%, var(--surface-2)); }
  .key.selected rect { stroke: var(--accent); stroke-width: 3; }
  .key text { font-size: 10px; text-anchor: middle; fill: var(--text-dim); pointer-events: none; }
  .key text.mm { font-size: 9px; fill: var(--text); }
  .ghost { padding: 0.25rem 0.55rem; font-size: 0.78rem; color: var(--text-dim); background: none; border: 1px solid var(--line); border-radius: 7px; cursor: pointer; }
  .ghost:disabled { opacity: 0.4; cursor: default; }
  .error { margin: 0.8rem 0 0; padding: 0.5rem 0.7rem; font-size: 0.8rem; color: var(--danger); background: color-mix(in srgb, var(--danger) 12%, transparent); border-radius: 8px; }
</style>
