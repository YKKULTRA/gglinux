<script lang="ts">
  // Panel for the Arctis Nova Pro Wireless: battery, and burn-in protection for
  // the base station's screen.
  //
  // Unlike the mouse, the base station reports its state, so what this shows are
  // readings, pushed by the daemon as they change.

  import Icon from "./Icon.svelte";
  import { daemon } from "./daemon.svelte";
  import type { HeadsetPower, ScreenSettings } from "./types";

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

  function setLitSeconds(raw: number) {
    if (!Number.isFinite(raw)) return;
    void update({ lit_seconds: Math.min(LIT_MAX, Math.max(LIT_MIN, Math.round(raw))) });
  }
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
          onchange={(e) => setLitSeconds(e.currentTarget.valueAsNumber)}
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
  .error {
    margin: 0.8rem 0 0;
    padding: 0.5rem 0.7rem;
    font-size: 0.8rem;
    color: var(--danger);
    background: color-mix(in srgb, var(--danger) 12%, transparent);
    border-radius: 8px;
  }
</style>
