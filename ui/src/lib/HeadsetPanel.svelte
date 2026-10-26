<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<script lang="ts">
  // Panel for the Arctis Nova Pro Wireless: battery, and burn-in protection for
  // the base station's screen.
  //
  // Unlike the mouse, the base station reports its state, so what this shows are
  // readings, pushed by the daemon as they change.

  import Icon from "./Icon.svelte";
  import { daemon } from "./daemon.svelte";
  import type {
    HeadsetChange,
    HeadsetPower,
    NoiseMode,
    RadioMode,
    ScreenSettings,
  } from "./types";

  /** Mirrors ScreenSettings::LIT_SECONDS and ::BRIGHTNESS in gg-ipc. */
  const LIT_MIN = 3;
  const LIT_MAX = 600;
  const BRIGHTNESS_MIN = 1;
  const BRIGHTNESS_MAX = 10;

  const POWER_LABEL: Record<HeadsetPower, string> = {
    online: "headset on",
    charging: "headset charging",
    offline: "headset off",
    unknown: "headset state unknown",
  };

  let headset = $derived(daemon.headset);
  let screen = $derived(daemon.headset?.screen ?? null);

  let busy = $state(false);
  let error = $state<string | null>(null);

  // Depends on `connected` so a daemon restart refetches.
  $effect(() => {
    if (!daemon.connected) return;
    void daemon.getHeadsetState().catch((e: unknown) => {
      error = e instanceof Error ? e.message : String(e);
    });
  });

  /** Send the current settings with one field changed. Each control applies at
   *  once: the changes are cheap and reversible, and the screen shows the result. */
  async function update(patch: Partial<ScreenSettings>) {
    if (!screen) return;
    busy = true;
    error = null;
    try {
      await daemon.setHeadsetScreen({ ...screen, ...patch });
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      busy = false;
    }
  }

  // Read back whatever happened: Svelte rewrites the box only when the stored
  // value changes, so an entry clamped to it, refused, or not a number would keep
  // showing what was typed.
  async function setLitSeconds(box: HTMLInputElement) {
    const raw = box.valueAsNumber;
    if (Number.isFinite(raw)) {
      await update({ lit_seconds: Math.min(LIT_MAX, Math.max(LIT_MIN, Math.round(raw))) });
    }
    if (screen) box.value = String(screen.lit_seconds);
  }

  // --- headset settings ------------------------------------------------------

  const NOISE_MODES: { value: NoiseMode; label: string }[] = [
    { value: "off", label: "Off" },
    { value: "transparency", label: "Transparency" },
    { value: "anc", label: "ANC" },
  ];
  const SIDETONE = ["Off", "Low", "Medium", "High"];
  const AUTO_OFF = [
    { value: 0, label: "Never" },
    { value: 1, label: "1 min" },
    { value: 5, label: "5 min" },
    { value: 10, label: "10 min" },
    { value: 15, label: "15 min" },
    { value: 30, label: "30 min" },
    { value: 60, label: "60 min" },
  ];
  const RADIO: { value: RadioMode; label: string }[] = [
    { value: "speed", label: "Speed" },
    { value: "range", label: "Range" },
  ];
  const EQ_PRESETS = ["Flat", "Bass Boost", "Focus", "Smiley", "Custom"];
  const EQ_CUSTOM = 4;
  /** Standard octave centres; the device's exact band frequencies are undocumented. */
  const BAND_LABELS = ["31", "62", "125", "250", "500", "1k", "2k", "4k", "8k", "16k"];

  let controls = $derived(daemon.headset?.controls ?? null);

  /** Working copy of the custom curve, seeded once from what was last applied. */
  let bands = $state<number[]>(Array(10).fill(0));
  let editingCustom = $state(false);
  let bandsSeeded = false;
  $effect(() => {
    const applied = controls?.eq_bands;
    if (bandsSeeded || !applied) return;
    bandsSeeded = true;
    bands = [...applied];
  });

  async function change(c: HeadsetChange) {
    busy = true;
    error = null;
    try {
      await daemon.setHeadset(c);
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      busy = false;
    }
  }

  function pickPreset(i: number) {
    if (i === EQ_CUSTOM) {
      editingCustom = true;
      return;
    }
    editingCustom = false;
    void change({ setting: "eq_preset", value: i });
  }

  const signed = (db: number) => (db > 0 ? `+${db}` : `${db}`);
</script>

<article class="panel">
  <header class="top">
    <div class="glyph"><Icon name="headset" size={26} /></div>
    <div class="ident">
      <h3>Arctis Nova Pro Wireless</h3>
      <p class="meta">
        {#if !headset?.connected}
          base station not connected
        {:else}
          {POWER_LABEL[headset.power]}
          {#if headset.headset_battery !== null}
            <span class="dot">·</span>
            <span class="battery" class:low={headset.headset_battery <= 25}>
              <span class="bar"><span class="fill" style:width="{headset.headset_battery}%"></span></span>
              {headset.headset_battery}%
            </span>
          {/if}
          <span class="dot">·</span>
          {headset.spare_battery === null ? "no spare battery" : `spare ${headset.spare_battery}%`}
        {/if}
      </p>
    </div>
  </header>

  {#if screen}
    <section>
      <div class="head">
        <h4>Screen burn-in protection</h4>
        {#if headset?.connected && screen.protect}
          <span class="state">{headset.screen_dark ? "screen dark" : "screen lit"}</span>
        {/if}
      </div>
      <label class="toggle">
        <input
          type="checkbox"
          checked={screen.protect}
          disabled={busy}
          onchange={(e) => update({ protect: e.currentTarget.checked })}
        />
        Keep the base-station screen dark, lighting it briefly for battery, headset and volume
        changes
      </label>
      <div class="row">
        <span class="label">Stay lit for</span>
        <input
          class="num"
          type="number"
          min={LIT_MIN}
          max={LIT_MAX}
          value={screen.lit_seconds}
          disabled={busy || !screen.protect}
          onchange={(e) => setLitSeconds(e.currentTarget)}
        />
        <span class="unit">seconds</span>
      </div>
      <div class="row">
        <span class="label">Brightness</span>
        <input
          type="range"
          min={BRIGHTNESS_MIN}
          max={BRIGHTNESS_MAX}
          step="1"
          value={screen.brightness ?? BRIGHTNESS_MAX}
          disabled={busy}
          onchange={(e) => update({ brightness: e.currentTarget.valueAsNumber })}
        />
        <span class="unit">{screen.brightness ?? "unchanged"}</span>
      </div>
      <p class="hint">
        No brightness switches the screen off — the lowest level is still lit — so protection
        blanks it with a black image instead. These settings reset when the daemon restarts.
      </p>
    </section>
  {/if}

  {#if headset?.connected && controls}
    <section>
      <div class="head"><h4>Noise control</h4></div>
      <div class="segmented">
        {#each NOISE_MODES as m (m.value)}
          <button
            class:on={controls.noise_mode === m.value}
            disabled={busy}
            onclick={() => change({ setting: "noise_mode", value: m.value })}>{m.label}</button
          >
        {/each}
      </div>
      {#if controls.noise_mode === "transparency"}
        <div class="row">
          <span class="label">Transparency</span>
          <input
            type="range"
            min="1"
            max="10"
            step="1"
            value={controls.transparency ?? 5}
            disabled={busy}
            onchange={(e) => change({ setting: "transparency", value: e.currentTarget.valueAsNumber })}
          />
          <span class="unit">{controls.transparency ?? "–"}</span>
        </div>
      {/if}
    </section>

    <section>
      <div class="head">
        <h4>Microphone</h4>
        {#if controls.mic_muted !== null}
          <span class="state">{controls.mic_muted ? "muted" : "live"}</span>
        {/if}
      </div>
      <div class="row">
        <span class="label">Sidetone</span>
        <div class="segmented">
          {#each SIDETONE as label, i (i)}
            <button
              class:on={controls.sidetone === i}
              disabled={busy}
              onclick={() => change({ setting: "sidetone", value: i })}>{label}</button
            >
          {/each}
        </div>
      </div>
      <div class="row">
        <span class="label">Mic volume</span>
        <input
          type="range"
          min="1"
          max="10"
          step="1"
          value={controls.mic_volume ?? 10}
          disabled={busy}
          onchange={(e) => change({ setting: "mic_volume", value: e.currentTarget.valueAsNumber })}
        />
        <span class="unit">{controls.mic_volume ?? "unknown"}</span>
      </div>
      <div class="row">
        <span class="label">Mute light</span>
        <input
          type="range"
          min="1"
          max="10"
          step="1"
          value={controls.mic_led ?? 10}
          disabled={busy}
          onchange={(e) => change({ setting: "mic_led", value: e.currentTarget.valueAsNumber })}
        />
        <span class="unit">{controls.mic_led ?? "–"}</span>
      </div>
      <p class="hint">
        The headset can't report sidetone or mic volume, so those show what was last set here
        ("unknown" until then).
      </p>
    </section>

    <section>
      <div class="head"><h4>Power &amp; radio</h4></div>
      <div class="row">
        <span class="label">Auto-off</span>
        <select
          value={controls.auto_off_minutes ?? ""}
          disabled={busy}
          onchange={(e) =>
            change({ setting: "auto_off_minutes", value: Number(e.currentTarget.value) })}
        >
          {#if controls.auto_off_minutes === null}<option value="">unknown</option>{/if}
          {#each AUTO_OFF as a (a.value)}<option value={a.value}>{a.label}</option>{/each}
        </select>
      </div>
      <div class="row">
        <span class="label">2.4 GHz</span>
        <div class="segmented">
          {#each RADIO as r (r.value)}
            <button
              class:on={controls.radio_mode === r.value}
              disabled={busy}
              onclick={() => change({ setting: "radio_mode", value: r.value })}>{r.label}</button
            >
          {/each}
        </div>
      </div>
    </section>

    <section>
      <div class="head"><h4>Equalizer</h4></div>
      <div class="segmented">
        {#each EQ_PRESETS as label, i (i)}
          <button
            class:on={controls.eq_preset === i || (i === EQ_CUSTOM && editingCustom)}
            disabled={busy}
            onclick={() => pickPreset(i)}>{label}</button
          >
        {/each}
      </div>
      {#if editingCustom || controls.eq_preset === EQ_CUSTOM}
        <ul class="bands">
          {#each bands as db, i (i)}
            <li>
              <span class="freq">{BAND_LABELS[i]}</span>
              <input type="range" min="-10" max="10" step="0.5" bind:value={bands[i]} disabled={busy} />
              <span class="db">{signed(db)} dB</span>
            </li>
          {/each}
        </ul>
        <button
          class="apply"
          disabled={busy}
          onclick={() => change({ setting: "eq_bands", value: bands.map(Number) })}
        >
          Apply custom EQ
        </button>
      {/if}
      <p class="hint">The base station can't report its EQ, so the preset shows what was last set here.</p>
    </section>
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
    gap: 1rem;
    align-items: center;
  }
  .glyph {
    flex: none;
    width: 46px;
    height: 46px;
    display: grid;
    place-items: center;
    border-radius: 10px;
    background: var(--surface-2);
    color: var(--accent);
  }
  .ident {
    min-width: 0;
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
  .dot {
    opacity: 0.5;
    margin: 0 0.2rem;
  }
  .battery {
    display: inline-flex;
    align-items: center;
    gap: 0.4rem;
    color: var(--ok);
  }
  .battery.low {
    color: var(--danger);
  }
  .bar {
    display: inline-block;
    width: 54px;
    height: 6px;
    border-radius: 999px;
    background: var(--surface-2);
    overflow: hidden;
  }
  .fill {
    display: block;
    height: 100%;
    background: currentColor;
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
  .state {
    font-size: 0.76rem;
    color: var(--text-dim);
  }
  .toggle {
    display: flex;
    align-items: flex-start;
    gap: 0.5rem;
    font-size: 0.85rem;
    cursor: pointer;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    margin-top: 0.7rem;
  }
  .label {
    width: 6.5rem;
    font-size: 0.82rem;
    color: var(--text-dim);
  }
  .num {
    width: 5rem;
    padding: 0.3rem 0.45rem;
    font-family: var(--mono);
    font-size: 0.85rem;
    color: var(--text);
    background: var(--surface-2);
    border: 1px solid var(--line);
    border-radius: 7px;
  }
  .unit {
    font-size: 0.78rem;
    color: var(--text-dim);
  }
  .hint {
    margin: 0.7rem 0 0;
    font-size: 0.76rem;
    color: var(--text-dim);
  }
  .segmented {
    display: inline-flex;
    flex-wrap: wrap;
    background: var(--surface-2);
    border: 1px solid var(--line);
    border-radius: 8px;
    overflow: hidden;
  }
  .segmented button {
    padding: 0.35rem 0.75rem;
    font-size: 0.82rem;
    color: var(--text-dim);
    background: none;
    border: none;
    cursor: pointer;
  }
  .segmented button.on {
    color: var(--bg);
    background: var(--accent);
    font-weight: 600;
  }
  .segmented button:disabled {
    cursor: default;
  }
  select {
    padding: 0.3rem 0.45rem;
    font-size: 0.85rem;
    color: var(--text);
    background: var(--surface-2);
    border: 1px solid var(--line);
    border-radius: 7px;
  }
  .bands {
    margin: 0.7rem 0 0;
    padding: 0;
    list-style: none;
  }
  .bands li {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    padding: 0.1rem 0;
  }
  .freq {
    width: 2.4rem;
    font-family: var(--mono);
    font-size: 0.76rem;
    color: var(--text-dim);
    text-align: right;
  }
  .bands input {
    flex: 1;
    max-width: 16rem;
  }
  .db {
    width: 4rem;
    font-family: var(--mono);
    font-size: 0.78rem;
    color: var(--text-dim);
  }
  .apply {
    margin-top: 0.75rem;
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
  .error {
    margin: 0.8rem 0 0;
    padding: 0.5rem 0.7rem;
    font-size: 0.8rem;
    color: var(--danger);
    background: color-mix(in srgb, var(--danger) 12%, transparent);
    border-radius: 8px;
  }
</style>
