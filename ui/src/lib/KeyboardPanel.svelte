<script lang="ts">
  // Apex Pro TKL Gen 3 lighting: give it to gglinux or leave it to the
  // keyboard, pick an effect, and paint keys for the static one.

  import { SvelteSet } from "svelte/reactivity";
  import Icon from "./Icon.svelte";
  import { daemon } from "./daemon.svelte";
  import type { Effect, KeyColor, Lighting, Reactive, ReactiveMode, Rgb } from "./types";

  const EFFECTS: { kind: Effect["kind"]; label: string }[] = [
    { kind: "static", label: "Static" },
    { kind: "breathing", label: "Breathing" },
    { kind: "wave", label: "Wave" },
    { kind: "spectrum", label: "Spectrum" },
    { kind: "visualizer", label: "Audio" },
  ];
  const MODES: { mode: ReactiveMode; label: string }[] = [
    { mode: "fade", label: "Fade" },
    { mode: "ripple", label: "Ripple" },
  ];
  const STATUS = { off: "waiting for the keyboard", reading: "on", no_access: "no access" } as const;
  /** Same words as the README and 61-gglinux-reactive.rules. */
  const ACCESS_WARNING =
    "Reactive lighting needs to see which keys you press. The optional udev rule lets any program " +
    "you run read what you type on the Apex while you're logged in. gglinux reads only while " +
    "reactive lighting is on, and keeps presses in memory only until their light fades.";
  /** Pixels per key unit in the picture. */
  const U = 40;

  let kb = $derived(daemon.keyboard);
  let lighting = $derived(kb?.lighting ?? null);
  let busy = $state(false);
  let error = $state<string | null>(null);

  /** Static colours being edited, by HID code. They follow the daemon's, so a
   *  profile or ggctl changing them shows here, except while there is paint not
   *  applied yet. */
  let painted = $state<Record<number, Rgb>>({});
  let dirty = $state(false);
  const selected = new SvelteSet<number>();
  let brush = $state("#00c8ff");

  $effect(() => {
    const l = lighting;
    if (dirty || !l) return;
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
  let period = $derived(lighting && "period_s" in lighting.effect ? lighting.effect.period_s : 4);

  /** Whether the change was applied. Each change sends the whole lighting, so the
   *  controls wait while one is in flight: a second change built from the old
   *  lighting would undo the first. */
  async function apply(patch: Partial<Lighting>): Promise<boolean> {
    if (!lighting) return false;
    busy = true;
    error = null;
    try {
      await daemon.setLighting({ ...lighting, ...patch });
      return true;
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : String(e);
      return false;
    } finally {
      busy = false;
    }
  }

  function setEffect(kind: Effect["kind"]) {
    let effect: Effect;
    if (kind === "static") effect = { kind };
    else if (kind === "visualizer") effect = { kind, sensitivity: 1 };
    else if (kind === "breathing") effect = { kind, color: rgbOf(brush), period_s: period };
    else effect = { kind, period_s: period };
    void apply({ effect });
  }

  function setPeriod(p: number) {
    if (!lighting || !("period_s" in lighting.effect)) return;
    void apply({ effect: { ...lighting.effect, period_s: p } });
  }

  function setReactive(patch: Partial<Reactive>) {
    if (!lighting) return;
    void apply({ reactive: { ...lighting.reactive, ...patch } });
  }

  function toggle(code: number) {
    if (selected.has(code)) selected.delete(code);
    else selected.add(code);
  }

  function paint(codes: number[]) {
    const c = rgbOf(brush);
    painted = { ...painted, ...Object.fromEntries(codes.map((code) => [code, c])) };
    dirty = true;
  }

  async function applyStatic() {
    const keys: KeyColor[] = Object.entries(painted).map(([code, color]) => ({ code: Number(code), color }));
    if (await apply({ effect: { kind: "static" }, keys })) dirty = false;
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
              disabled={busy}
              onchange={(e) =>
                apply({ effect: { kind: "breathing", color: rgbOf(e.currentTarget.value), period_s: period } })}
            />
          </div>
        {/if}
        {#if "period_s" in lighting.effect}
          <div class="row">
            <span class="label">Speed</span>
            <input
              type="range"
              min="0.5"
              max="20"
              step="0.5"
              value={period}
              disabled={busy}
              onchange={(e) => setPeriod(e.currentTarget.valueAsNumber)}
            />
            <span class="unit">{period} s</span>
          </div>
        {/if}
        {#if lighting.effect.kind === "visualizer"}
          {@const sens = lighting.effect.sensitivity}
          <div class="row">
            <span class="label">Sensitivity</span>
            <input
              type="range"
              min="0.25"
              max="4"
              step="0.25"
              value={sens}
              disabled={busy}
              onchange={(e) => apply({ effect: { kind: "visualizer", sensitivity: e.currentTarget.valueAsNumber } })}
            />
            <span class="unit">{sens.toFixed(2)}×</span>
          </div>
          <p class="hint">What's playing is analysed live and never recorded; KDE may show that audio is being captured.</p>
        {/if}
        <div class="row">
          <span class="label">Brightness</span>
          <input
            type="range"
            min="0"
            max="100"
            step="5"
            value={lighting.brightness}
            disabled={busy}
            onchange={(e) => apply({ brightness: e.currentTarget.valueAsNumber })}
          />
          <span class="unit">{lighting.brightness}%</span>
        </div>
      </section>

      <section>
        <div class="head">
          <h4>Reactive</h4>
          {#if lighting.reactive.enabled}<span class="unit">{STATUS[kb.reactive]}</span>{/if}
        </div>
        <label class="toggle">
          <input
            type="checkbox"
            checked={lighting.reactive.enabled}
            disabled={busy}
            onchange={(e) => setReactive({ enabled: e.currentTarget.checked })}
          />
          Light keys as you press them
        </label>
        {#if lighting.reactive.enabled}
          <div class="row">
            <div class="segmented">
              {#each MODES as m (m.mode)}
                <button class:on={lighting.reactive.mode === m.mode} disabled={busy} onclick={() => setReactive({ mode: m.mode })}>
                  {m.label}
                </button>
              {/each}
            </div>
          </div>
          <div class="row">
            <span class="label">Colour</span>
            <input
              type="color"
              value={hex(lighting.reactive.color)}
              disabled={busy}
              onchange={(e) => setReactive({ color: rgbOf(e.currentTarget.value) })}
            />
          </div>
          <div class="row">
            <span class="label">Duration</span>
            <input
              type="range"
              min="0.1"
              max="3"
              step="0.1"
              value={lighting.reactive.duration_s}
              disabled={busy}
              onchange={(e) => setReactive({ duration_s: e.currentTarget.valueAsNumber })}
            />
            <span class="unit">{lighting.reactive.duration_s.toFixed(1)} s</span>
          </div>
          {#if kb.reactive === "no_access"}
            <div class="notice">
              <p>{ACCESS_WARNING}</p>
              <!-- Just the rule: install-local.sh would also put a ~/.local daemon before a packaged one. -->
              <p>To allow it, add the optional rule, then replug the keyboard:</p>
              <pre><code>sudo install -m644 /usr/share/gglinux/61-gglinux-reactive.rules \
  /etc/udev/rules.d/
sudo udevadm control --reload</code></pre>
              <p>Built from source? Copy <code>data/udev/61-gglinux-reactive.rules</code> from the checkout instead.</p>
            </div>
          {/if}
        {/if}
      </section>

      <section>
        <div class="head"><h4>Mic mute light</h4></div>
        <label class="toggle">
          <input
            type="checkbox"
            checked={lighting.mic_mute.enabled}
            disabled={busy}
            onchange={(e) => apply({ mic_mute: { ...lighting!.mic_mute, enabled: e.currentTarget.checked } })}
          />
          Light a key while the microphone is muted
        </label>
        {#if lighting.mic_mute.enabled}
          <div class="row">
            <span class="label">Key</span>
            <select
              value={lighting.mic_mute.key}
              disabled={busy}
              onchange={(e) => apply({ mic_mute: { ...lighting!.mic_mute, key: Number(e.currentTarget.value) } })}
            >
              {#each kb.layout.filter((k) => k.label) as k (k.code)}
                <option value={k.code}>{k.label}</option>
              {/each}
            </select>
            <input
              type="color"
              value={hex(lighting.mic_mute.color)}
              disabled={busy}
              onchange={(e) => apply({ mic_mute: { ...lighting!.mic_mute, color: rgbOf(e.currentTarget.value) } })}
            />
          </div>
        {/if}
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
  .notice { margin-top: 0.8rem; padding: 0.55rem 0.75rem; font-size: 0.8rem; color: var(--text-dim); background: var(--surface-2); border: 1px solid var(--line); border-radius: 8px; }
  .notice p { margin: 0; }
  .notice p + p { margin-top: 0.45rem; }
  .notice code { font-size: 0.78rem; color: var(--text); }
  .notice pre { margin: 0.45rem 0; padding: 0.45rem 0.6rem; overflow-x: auto; background: var(--bg); border: 1px solid var(--line); border-radius: 6px; }
  .hint { margin: 0.5rem 0 0; font-size: 0.78rem; color: var(--text-dim); }
  .error { margin: 0.8rem 0 0; padding: 0.5rem 0.7rem; font-size: 0.8rem; color: var(--danger); background: color-mix(in srgb, var(--danger) 12%, transparent); border-radius: 8px; }
</style>
