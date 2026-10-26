<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<script lang="ts">
  // Control panel for the Rival 3 Wireless Gen 2.
  //
  // The vendor protocol is write-only — there is no command that reads settings
  // back off the mouse — so this panel is careful to distinguish two things it
  // would be easy to conflate: the *form*, which is what will be written, and
  // `daemon.mouse.applied`, which is what this daemon has written since it
  // started. Neither is a reading of the hardware, and the panel never claims to
  // show one.

  import Icon from "./Icon.svelte";
  import { daemon } from "./daemon.svelte";
  import type { MouseSettings } from "./types";

  /** Mirrors gg-device-mouse: DPI_MIN/DPI_MAX/DPI_STEP and MAX_PRESETS. */
  const DPI_MIN = 100;
  const DPI_MAX = 18000;
  const DPI_STEP = 100;
  const MAX_STAGES = 5;

  /** The rates PollingRate::from_hz accepts; anything else is rejected daemon-side. */
  const POLLING_RATES = [125, 250, 500, 1000];

  /** Physical button names for the Rival 3 layout, used only as row labels. The
   *  length is the protocol's BUTTON_COUNT: the daemon rejects any other count. */
  const BUTTON_LABELS = ["Left", "Right", "Middle", "Back", "Forward", "DPI"];

  /** The subset of ButtonAction::parse names worth exposing as a menu. The parser
   *  also takes any key name from its keyboard table, which a text field would
   *  reach — deliberately out of scope until there is a key-capture control. */
  const ACTIONS: { value: string; label: string }[] = [
    { value: "button1", label: "Left click" },
    { value: "button2", label: "Right click" },
    { value: "button3", label: "Middle click" },
    { value: "button4", label: "Back" },
    { value: "button5", label: "Forward" },
    { value: "dpi", label: "Cycle DPI" },
    { value: "scrollup", label: "Scroll up" },
    { value: "scrolldown", label: "Scroll down" },
    { value: "playpause", label: "Play / pause" },
    { value: "next", label: "Next track" },
    { value: "previous", label: "Previous track" },
    { value: "mute", label: "Mute" },
    { value: "volumeup", label: "Volume up" },
    { value: "volumedown", label: "Volume down" },
    { value: "disable", label: "Disabled" },
  ];

  /** proto::default_buttons() — each button is itself, except 6 which cycles DPI. */
  const FACTORY_BUTTONS = ["button1", "button2", "button3", "button4", "button5", "dpi"];

  // --- form state (what will be written) -----------------------------------

  let stages = $state<number[]>([800, 1600]);
  let active = $state(0);
  let polling = $state(1000);
  let buttons = $state<string[]>([...FACTORY_BUTTONS]);

  /** Commit to onboard memory as well as applying, for every write below. */
  let persist = $state(false);

  let busy = $state<string | null>(null);
  let error = $state<string | null>(null);
  let seeded = false;

  let mouse = $derived(daemon.mouse);
  let applied = $derived(daemon.mouse?.applied ?? null);

  // Seed the form from whatever the daemon has already applied, once. After that
  // the form is the user's working copy and must not be overwritten underneath
  // them by an unrelated refresh. A part not written yet keeps the form's own.
  $effect(() => {
    const a = applied;
    if (seeded || !a) return;
    seeded = true;
    if (a.dpi) {
      stages = a.dpi.stages.map((s) => s.x);
      active = a.dpi.active;
    }
    if (a.polling_hz !== null) polling = a.polling_hz;
    if (a.buttons) buttons = [...a.buttons];
  });

  // Depends on `connected` so a daemon restart refetches rather than leaving the
  // panel showing state from a process that no longer exists.
  $effect(() => {
    if (!daemon.connected) return;
    void daemon.getMouseState().catch((e: unknown) => {
      error = e instanceof Error ? e.message : String(e);
    });
  });

  const clampDpi = (n: number) =>
    Math.min(DPI_MAX, Math.max(DPI_MIN, Math.round(n / DPI_STEP) * DPI_STEP));

  // Svelte rewrites the box only when the stage changes, so an entry that rounds
  // or clamps to the stage it already was, or isn't a number, would keep showing
  // what was typed. Write back what will be applied.
  function setStage(i: number, box: HTMLInputElement) {
    const raw = box.valueAsNumber;
    if (Number.isFinite(raw)) stages[i] = clampDpi(raw);
    box.value = String(stages[i]);
  }

  function addStage() {
    if (stages.length >= MAX_STAGES) return;
    stages = [...stages, stages[stages.length - 1] ?? 800];
  }

  function removeStage(i: number) {
    if (stages.length <= 1) return;
    stages = stages.filter((_, n) => n !== i);
    // The selected stage moves down a place when one before it goes. And the
    // daemon rejects an out-of-range active index, which would be a confusing
    // error to surface for an edit the user made somewhere else.
    if (i < active) active -= 1;
    else if (active >= stages.length) active = stages.length - 1;
  }

  /** Run one write, keeping the button that triggered it disabled meanwhile. */
  async function run(tag: string, op: () => Promise<void>) {
    busy = tag;
    error = null;
    try {
      await op();
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      busy = null;
    }
  }

  const applyDpi = () =>
    run("dpi", () =>
      daemon.setMouseDpi(
        stages.map((x) => ({ x, y: x })),
        active,
        persist,
      ),
    );

  const applyPolling = () => run("polling", () => daemon.setMousePolling(polling, persist));
  const applyButtons = () => run("buttons", () => daemon.setMouseButtons(buttons, persist));
  const saveOnboard = () => run("save", () => daemon.saveMouse());

  /** Only the parts written: the others are unknown, so they aren't shown. */
  const summarise = (a: MouseSettings) =>
    [
      a.dpi && `${a.dpi.stages.map((s) => s.x).join(" / ")} dpi · stage ${a.dpi.active + 1}`,
      a.polling_hz !== null && `${a.polling_hz} Hz`,
      a.buttons && "button layout",
    ]
      .filter(Boolean)
      .join(" · ");
</script>

<article class="panel">
  <header class="top">
    <div class="glyph"><Icon name="mouse" size={26} /></div>
    <div class="ident">
      <h3>Rival 3 Wireless Gen 2</h3>
      <p class="meta">
        {#if mouse?.battery}
          <span class="battery" class:low={mouse.battery.level <= 20}>
            <span class="bar"><span class="fill" style:width="{mouse.battery.level}%"></span></span>
            {mouse.battery.level}%
          </span>
          <span class="dot">·</span>
          {mouse.battery.charging ? "charging" : "on battery"}
        {:else if mouse?.connected}
          battery unknown — the mouse reports nothing while asleep
        {:else}
          not connected
        {/if}
      </p>
    </div>
  </header>

  <p class="caveat">
    This mouse has no way to report its settings back, so nothing here is read from the
    hardware.
    {#if applied}
      Applied this session: <strong>{summarise(applied)}</strong>.
    {:else}
      Nothing has been written this session — the mouse is running its onboard configuration.
    {/if}
  </p>

  <section>
    <div class="head">
      <h4>DPI stages</h4>
      <button class="ghost" onclick={addStage} disabled={stages.length >= MAX_STAGES}>
        Add stage
      </button>
    </div>
    <ul class="stages">
      {#each stages as dpi, i (i)}
        <li>
          <label class="pick">
            <input type="radio" name="active-stage" value={i} bind:group={active} />
            <span class="idx">{i + 1}</span>
          </label>
          <input
            class="num"
            type="number"
            min={DPI_MIN}
            max={DPI_MAX}
            step={DPI_STEP}
            value={dpi}
            onchange={(e) => setStage(i, e.currentTarget)}
          />
          <span class="unit">dpi</span>
          <button
            class="ghost remove"
            onclick={() => removeStage(i)}
            disabled={stages.length <= 1}
            aria-label="Remove stage {i + 1}"
          >
            &times;
          </button>
        </li>
      {/each}
    </ul>
    <p class="hint">The selected stage is the one the mouse switches to.</p>
    <button class="apply" onclick={applyDpi} disabled={busy !== null}>
      {busy === "dpi" ? "Applying…" : "Apply DPI"}
    </button>
  </section>

  <section>
    <div class="head"><h4>Polling rate</h4></div>
    <div class="segmented">
      {#each POLLING_RATES as hz (hz)}
        <button class:on={polling === hz} onclick={() => (polling = hz)}>{hz} Hz</button>
      {/each}
    </div>
    <button class="apply" onclick={applyPolling} disabled={busy !== null}>
      {busy === "polling" ? "Applying…" : "Apply polling rate"}
    </button>
  </section>

  <section>
    <div class="head">
      <h4>Buttons</h4>
      <button class="ghost" onclick={() => (buttons = [...FACTORY_BUTTONS])}>Reset to factory</button>
    </div>
    <ul class="buttons">
      {#each BUTTON_LABELS as label, i (i)}
        <li>
          <span class="blabel">{label}</span>
          <select bind:value={buttons[i]}>
            {#each ACTIONS as a (a.value)}
              <option value={a.value}>{a.label}</option>
            {/each}
          </select>
        </li>
      {/each}
    </ul>
    <button class="apply" onclick={applyButtons} disabled={busy !== null}>
      {busy === "buttons" ? "Applying…" : "Apply buttons"}
    </button>
  </section>

  <footer>
    <label class="persist">
      <input type="checkbox" bind:checked={persist} />
      Write to onboard memory as well
    </label>
    <p class="hint">
      Without this, settings are lost when the mouse sleeps. With it, they survive — and
      overwrite what was stored, which cannot be recovered.
    </p>
    <button class="apply" onclick={saveOnboard} disabled={busy !== null}>
      {busy === "save" ? "Saving…" : "Save current settings to mouse"}
    </button>
  </footer>

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

  .caveat {
    margin: 0.9rem 0 0;
    padding: 0.55rem 0.7rem;
    font-size: 0.79rem;
    color: var(--text-dim);
    background: var(--surface-2);
    border-radius: 8px;
  }
  .caveat strong {
    color: var(--text);
    font-weight: 500;
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
  .hint {
    margin: 0.45rem 0 0;
    font-size: 0.76rem;
    color: var(--text-dim);
  }

  ul {
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .stages li {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    padding: 0.2rem 0;
  }
  .pick {
    display: inline-flex;
    align-items: center;
    gap: 0.4rem;
    cursor: pointer;
  }
  .idx {
    font-size: 0.78rem;
    color: var(--text-dim);
    width: 0.8rem;
  }
  .num {
    width: 6.2rem;
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

  .buttons li {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    padding: 0.2rem 0;
  }
  .blabel {
    width: 4.6rem;
    font-size: 0.82rem;
    color: var(--text-dim);
  }
  select {
    flex: 1;
    max-width: 14rem;
    padding: 0.3rem 0.45rem;
    font-size: 0.85rem;
    color: var(--text);
    background: var(--surface-2);
    border: 1px solid var(--line);
    border-radius: 7px;
  }

  .segmented {
    display: inline-flex;
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

  .ghost {
    padding: 0.2rem 0.5rem;
    font-size: 0.76rem;
    color: var(--text-dim);
    background: none;
    border: 1px solid var(--line);
    border-radius: 7px;
    cursor: pointer;
  }
  .ghost:hover:not(:disabled) {
    color: var(--text);
    border-color: var(--text-dim);
  }
  .ghost:disabled {
    opacity: 0.4;
    cursor: default;
  }
  .remove {
    border-color: transparent;
  }
  .remove:hover:not(:disabled) {
    color: var(--danger);
    border-color: var(--danger);
  }

  footer {
    margin-top: 1.1rem;
    padding-top: 1rem;
    border-top: 1px solid var(--line);
  }
  .persist {
    display: inline-flex;
    align-items: center;
    gap: 0.45rem;
    font-size: 0.85rem;
    cursor: pointer;
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
