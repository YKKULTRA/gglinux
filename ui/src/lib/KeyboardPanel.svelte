<script lang="ts">
  // Apex Pro TKL Gen 3 lighting: give it to gglinux or leave it to the
  // keyboard, pick an effect, and paint keys for the static one.

  import { SvelteSet } from "svelte/reactivity";
  import Icon from "./Icon.svelte";
  import { daemon } from "./daemon.svelte";
  import type { Effect, KeyColor, Lighting, Rgb } from "./types";

  const EFFECTS: { kind: Effect["kind"]; label: string }[] = [
    { kind: "static", label: "Static" },
    { kind: "breathing", label: "Breathing" },
    { kind: "wave", label: "Wave" },
    { kind: "spectrum", label: "Spectrum" },
  ];
  /** Pixels per key unit in the picture. */
  const U = 40;

  let kb = $derived(daemon.keyboard);
  let lighting = $derived(kb?.lighting ?? null);
  let busy = $state(false);
  let error = $state<string | null>(null);

  /** Static colours being edited, by HID code; seeded once from the daemon. */
  let painted = $state<Record<number, Rgb>>({});
  let seeded = false;
  const selected = new SvelteSet<number>();
  let brush = $state("#00c8ff");

  $effect(() => {
    const l = lighting;
    if (seeded || !l) return;
    seeded = true;
    painted = Object.fromEntries(l.keys.map((k) => [k.code, k.color]));
  });

  $effect(() => {
    if (!daemon.connected) return;
    void daemon.getKeyboardState().catch((e: unknown) => {
      error = e instanceof Error ? e.message : String(e);
    });
  });

  const hex = (c: Rgb) => "#" + [c.r, c.g, c.b].map((v) => v.toString(16).padStart(2, "0")).join("");
  const rgbOf = (h: string): Rgb => ({
    r: parseInt(h.slice(1, 3), 16),
    g: parseInt(h.slice(3, 5), 16),
    b: parseInt(h.slice(5, 7), 16),
  });
  let width = $derived(kb ? Math.max(...kb.layout.map((k) => k.x + k.w)) * U : 0);
  let height = $derived(kb ? Math.max(...kb.layout.map((k) => k.y + 1)) * U : 0);
  let period = $derived(lighting && lighting.effect.kind !== "static" ? lighting.effect.period_s : 4);

  async function apply(patch: Partial<Lighting>) {
    if (!lighting) return;
    busy = true;
    error = null;
    try {
      await daemon.setLighting({ ...lighting, ...patch });
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      busy = false;
    }
  }

  function setEffect(kind: Effect["kind"]) {
    const effect: Effect =
      kind === "static"
        ? { kind }
        : kind === "breathing"
          ? { kind, color: rgbOf(brush), period_s: period }
          : { kind, period_s: period };
    void apply({ effect });
  }

  function setPeriod(p: number) {
    if (!lighting || lighting.effect.kind === "static") return;
    void apply({ effect: { ...lighting.effect, period_s: p } });
  }

  function toggle(code: number) {
    if (selected.has(code)) selected.delete(code);
    else selected.add(code);
  }

  function paint(codes: number[]) {
    const c = rgbOf(brush);
    painted = { ...painted, ...Object.fromEntries(codes.map((code) => [code, c])) };
  }

  function applyStatic() {
    const keys: KeyColor[] = Object.entries(painted).map(([code, color]) => ({ code: Number(code), color }));
    void apply({ effect: { kind: "static" }, keys });
  }
</script>

<article class="panel">
  <header class="top">
    <div class="glyph"><Icon name="keyboard" size={26} /></div>
    <div class="ident">
      <h3>Apex Pro TKL Gen 3</h3>
      <p class="meta">{kb?.connected ? "connected" : "not connected"}</p>
    </div>
  </header>

  {#if lighting && kb}
    <section>
      <label class="toggle">
        <input
          type="checkbox"
          checked={lighting.enabled}
          disabled={busy}
          onchange={(e) => apply({ enabled: e.currentTarget.checked })}
        />
        gglinux controls the lighting (off: the keyboard's own profile)
      </label>
    </section>

    {#if lighting.enabled}
      <section>
        <div class="head"><h4>Effect</h4></div>
        <div class="segmented">
          {#each EFFECTS as e (e.kind)}
            <button class:on={lighting.effect.kind === e.kind} disabled={busy} onclick={() => setEffect(e.kind)}>
              {e.label}
            </button>
          {/each}
        </div>
        {#if lighting.effect.kind === "breathing"}
          <div class="row">
            <span class="label">Colour</span>
            <input
              type="color"
              value={hex(lighting.effect.color)}
              onchange={(e) =>
                apply({ effect: { kind: "breathing", color: rgbOf(e.currentTarget.value), period_s: period } })}
            />
          </div>
        {/if}
        {#if lighting.effect.kind !== "static"}
          <div class="row">
            <span class="label">Speed</span>
            <input
              type="range"
              min="0.5"
              max="20"
              step="0.5"
              value={period}
              onchange={(e) => setPeriod(e.currentTarget.valueAsNumber)}
            />
            <span class="unit">{period} s</span>
          </div>
        {/if}
        <div class="row">
          <span class="label">Brightness</span>
          <input
            type="range"
            min="0"
            max="100"
            step="5"
            value={lighting.brightness}
            onchange={(e) => apply({ brightness: e.currentTarget.valueAsNumber })}
          />
          <span class="unit">{lighting.brightness}%</span>
        </div>
      </section>

      {#if lighting.effect.kind === "static"}
        <section>
          <div class="head"><h4>Keys</h4></div>
          <svg class="board" viewBox="-4 -4 {width + 8} {height + 8}" role="group" aria-label="Keyboard">
            {#each kb.layout as k (k.code)}
              <g class="key" class:selected={selected.has(k.code)}>
                <rect
                  x={k.x * U + 2}
                  y={k.y * U + 2}
                  width={k.w * U - 4}
                  height={U - 4}
                  rx="5"
                  fill={painted[k.code] ? hex(painted[k.code]) : "var(--surface-2)"}
                  role="button"
                  tabindex="-1"
                  aria-label={k.label || "Space"}
                  onclick={() => toggle(k.code)}
                  onkeydown={() => {}}
                />
                <text x={k.x * U + (k.w * U) / 2} y={k.y * U + U / 2 + 4}>{k.label}</text>
              </g>
            {/each}
          </svg>
          <div class="row">
            <input type="color" bind:value={brush} />
            <button class="ghost" disabled={selected.size === 0} onclick={() => paint([...selected])}>
              Paint selected ({selected.size})
            </button>
            <button class="ghost" onclick={() => paint(kb!.layout.map((k) => k.code))}>Paint all</button>
            <button class="ghost" onclick={() => selected.clear()}>Clear selection</button>
          </div>
          <button class="apply" disabled={busy} onclick={applyStatic}>Apply to keyboard</button>
        </section>
      {/if}
    {/if}
  {/if}

  {#if error}
    <p class="error">{error}</p>
  {/if}
</article>

<style>
  .panel { padding: 1rem 1.1rem; background: var(--surface); border: 1px solid var(--line); border-radius: 12px; }
  .top { display: flex; gap: 1rem; align-items: center; }
  .glyph { flex: none; width: 46px; height: 46px; display: grid; place-items: center; border-radius: 10px; background: var(--surface-2); color: var(--accent); }
  h3 { margin: 0; font-size: 0.98rem; font-weight: 600; }
  h4 { margin: 0; font-size: 0.8rem; font-weight: 600; text-transform: uppercase; letter-spacing: 0.05em; color: var(--text-dim); }
  .meta { margin: 0.25rem 0 0; font-size: 0.82rem; color: var(--text-dim); }
  section { margin-top: 1.1rem; padding-top: 1rem; border-top: 1px solid var(--line); }
  .head { display: flex; align-items: center; justify-content: space-between; gap: 0.6rem; margin-bottom: 0.6rem; }
  .toggle { display: flex; align-items: center; gap: 0.5rem; font-size: 0.85rem; cursor: pointer; }
  .row { display: flex; align-items: center; gap: 0.6rem; margin-top: 0.7rem; flex-wrap: wrap; }
  .label { width: 5.5rem; font-size: 0.82rem; color: var(--text-dim); }
  .unit { font-size: 0.78rem; color: var(--text-dim); }
  .segmented { display: inline-flex; flex-wrap: wrap; background: var(--surface-2); border: 1px solid var(--line); border-radius: 8px; overflow: hidden; }
  .segmented button { padding: 0.35rem 0.75rem; font-size: 0.82rem; color: var(--text-dim); background: none; border: none; cursor: pointer; }
  .segmented button.on { color: var(--bg); background: var(--accent); font-weight: 600; }
  .board { width: 100%; max-width: 48rem; height: auto; user-select: none; }
  .key rect { stroke: var(--line); stroke-width: 1.5; cursor: pointer; }
  .key.selected rect { stroke: var(--accent); stroke-width: 3; }
  .key text { font-size: 10px; text-anchor: middle; fill: var(--text-dim); pointer-events: none; }
  .ghost { padding: 0.25rem 0.55rem; font-size: 0.78rem; color: var(--text-dim); background: none; border: 1px solid var(--line); border-radius: 7px; cursor: pointer; }
  .ghost:disabled { opacity: 0.4; cursor: default; }
  .apply { margin-top: 0.75rem; padding: 0.4rem 0.85rem; font-size: 0.83rem; font-weight: 500; color: var(--text); background: var(--surface-2); border: 1px solid var(--line); border-radius: 8px; cursor: pointer; }
  .apply:hover:not(:disabled) { border-color: var(--accent); color: var(--accent); }
  .apply:disabled { opacity: 0.5; cursor: default; }
  .error { margin: 0.8rem 0 0; padding: 0.5rem 0.7rem; font-size: 0.8rem; color: var(--danger); background: color-mix(in srgb, var(--danger) 12%, transparent); border-radius: 8px; }
</style>
