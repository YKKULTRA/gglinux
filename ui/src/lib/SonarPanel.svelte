<script lang="ts">
  // Sonar: Game / Chat / Media / Aux channels hosted by PipeWire, apps routed
  // into them by rule, and ChatMix between Game and Chat.

  import EqEditor from "./EqEditor.svelte";
  import { daemon } from "./daemon.svelte";
  import type { Channel, EqBand, MicSettings, SmartLevel } from "./types";

  const CHANNELS: { value: Channel; label: string }[] = [
    { value: "game", label: "Game" },
    { value: "chat", label: "Chat" },
    { value: "media", label: "Media" },
    { value: "aux", label: "Aux" },
  ];
  const LEVELS: { value: SmartLevel; label: string }[] = [
    { value: "quiet", label: "Quiet" },
    { value: "balanced", label: "Balanced" },
    { value: "loud", label: "Loud" },
  ];
  const label = (c: Channel) => CHANNELS.find((x) => x.value === c)?.label ?? c;

  let sonar = $derived(daemon.sonar);
  let busy = $state<string | null>(null);
  let error = $state<string | null>(null);

  $effect(() => {
    if (!daemon.connected) return;
    void daemon.getSonarState().catch((e: unknown) => {
      error = e instanceof Error ? e.message : String(e);
    });
  });

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

  const pct = (v: number | null) => (v === null ? "–" : `${Math.round(v * 100)}%`);

  type EqTab = Channel | "headphone";
  let eqTab = $state<EqTab>("game");
  let importText = $state("");
  let importName = $state("");

  /** EQ edits skip `busy`, so a drag isn't cut short by its own request. */
  async function quietly(op: () => Promise<void>) {
    error = null;
    try {
      await op();
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  const flatten = (bands: EqBand[]) => bands.map((b) => ({ ...b, gain: 0 }));
  let tabBands = $derived(
    !sonar ? [] : eqTab === "headphone" ? sonar.headphone.bands : (sonar.eq.find((c) => c.channel === eqTab)?.bands ?? []),
  );

  function setBands(bands: EqBand[]) {
    const s = sonar;
    if (!s) return;
    void quietly(() =>
      eqTab === "headphone"
        ? daemon.setHeadphoneEq({ ...s.headphone, bands })
        : daemon.setChannelEq(eqTab, bands),
    );
  }

  function micSettings(patch: Partial<MicSettings>) {
    const s = sonar;
    if (!s) return;
    void quietly(() => daemon.setMicSettings({ ...s.mic.settings, ...patch }));
  }

  function importAutoEq() {
    const text = importText;
    const name = importName.trim() || null;
    void run("import", async () => {
      await daemon.importAutoEq(text, name);
      importText = "";
    });
  }
</script>

<article class="panel">
  {#if !sonar}
    <p class="hint">Loading…</p>
  {:else if !sonar.available}
    <p class="hint">PipeWire isn't reachable, so Sonar can't run.</p>
  {:else}
    <header class="top">
      <div>
        <h3>Sonar</h3>
        <p class="meta">
          {sonar.enabled
            ? "Channels are live. Apps are routed by the rules below."
            : "Off. Your apps play straight to your default output."}
        </p>
      </div>
      <button
        class="apply"
        disabled={busy !== null}
        onclick={() => run("enable", () => daemon.setSonarEnabled(!sonar!.enabled))}
      >
        {busy === "enable" ? "Restarting PipeWire…" : sonar.enabled ? "Turn off" : "Turn on"}
      </button>
    </header>
    <p class="hint">Turning Sonar on or off restarts PipeWire: a second or two of silence.</p>

    <section>
      <div class="row">
        <span class="label">Output</span>
        <select
          value={sonar.output ?? ""}
          disabled={busy !== null}
          onchange={(e) => run("output", () => daemon.setSonarOutput(e.currentTarget.value))}
        >
          {#if !sonar.output}<option value="">chosen when Sonar turns on</option>{/if}
          {#each sonar.outputs as o (o.name)}<option value={o.name}>{o.description}</option>{/each}
        </select>
      </div>
    </section>

    {#if sonar.enabled}
      <section>
        <div class="head"><h4>Channels</h4></div>
        {#each sonar.channels as c (c.channel)}
          <div class="row">
            <span class="label">{label(c.channel)}</span>
            <input
              type="range"
              min="0"
              max="100"
              step="1"
              value={Math.round((c.volume ?? 1) * 100)}
              disabled={!c.present || busy !== null}
              onchange={(e) =>
                run("volume", () => daemon.setChannelVolume(c.channel, e.currentTarget.valueAsNumber / 100))}
            />
            <span class="unit">{c.present ? pct(c.volume) : "missing"}</span>
          </div>
        {/each}
      </section>

      <section>
        <div class="head"><h4>Smart Volume</h4></div>
        {#if !sonar.smart_volume.available}
          <!-- The daemon says why: swh-plugins is missing (it asks for it), or PipeWire refused the stage. -->
          <p class="hint">{sonar.smart_volume.reason ?? "Smart Volume isn't available."}</p>
        {:else}
          {#each sonar.channels as c, i (c.channel)}
            {@const sv = sonar.smart_volume.settings[i]}
            <div class="row">
              <span class="label">{label(c.channel)}</span>
              <label class="check inline">
                <input
                  type="checkbox"
                  checked={sv.enabled}
                  onchange={(e) =>
                    quietly(() => daemon.setSmartVolume(c.channel, { ...sv, enabled: e.currentTarget.checked }))}
                />
                On
              </label>
              <select
                value={sv.level}
                disabled={!sv.enabled}
                onchange={(e) =>
                  quietly(() =>
                    daemon.setSmartVolume(c.channel, { ...sv, level: e.currentTarget.value as SmartLevel }),
                  )}
              >
                {#each LEVELS as l (l.value)}<option value={l.value}>{l.label}</option>{/each}
              </select>
            </div>
          {/each}
          <p class="hint">
            Evens out a channel's loudness: quiet sounds come up, loud ones are held down.
            Switching is instant.
          </p>
        {/if}
      </section>

      <section>
        <div class="head">
          <h4>Streamer mode</h4>
          {#if busy === "streamer"}<span class="unit wide">Restarting PipeWire…</span>{/if}
        </div>
        <label class="check">
          <input
            type="checkbox"
            checked={sonar.stream.enabled}
            disabled={busy !== null}
            onchange={(e) => run("streamer", () => daemon.setStreamer(e.currentTarget.checked))}
          />
          A separate mix for your stream
        </label>
        <p class="hint">Turning this on or off restarts PipeWire: a second or two of silence.</p>
        {#if sonar.stream.enabled}
          {#if !sonar.stream.present}
            <p class="hint warn">PipeWire hasn't created the stream device yet.</p>
          {/if}
          {#each sonar.channels as c, i (c.channel)}
            <div class="row">
              <span class="label">{label(c.channel)}</span>
              <input
                type="range"
                min="0"
                max="100"
                step="1"
                value={Math.round(sonar.stream.volumes[i] * 100)}
                onchange={(e) =>
                  quietly(() => daemon.setStreamVolume(c.channel, e.currentTarget.valueAsNumber / 100))}
              />
              <span class="unit">{pct(sonar.stream.volumes[i])}</span>
            </div>
          {/each}
          <div class="row">
            <span class="label">Master</span>
            <input
              type="range"
              min="0"
              max="100"
              step="1"
              value={Math.round(sonar.stream.master * 100)}
              onchange={(e) => quietly(() => daemon.setStreamMaster(e.currentTarget.valueAsNumber / 100))}
            />
            <span class="unit">{pct(sonar.stream.master)}</span>
          </div>
          <p class="hint">In OBS, add an Audio Output Capture of <em>Monitor of gglinux Stream</em>.</p>
        {/if}
      </section>

      <section>
        <div class="head">
          <h4>ChatMix</h4>
          <button class="ghost" disabled={busy !== null} onclick={() => run("chatmix", () => daemon.setChatMix(0))}>
            Centre
          </button>
        </div>
        <div class="row">
          <span class="label">Game</span>
          <input
            type="range"
            min="-100"
            max="100"
            step="5"
            value={Math.round(sonar.chatmix * 100)}
            disabled={busy !== null}
            onchange={(e) => run("chatmix", () => daemon.setChatMix(e.currentTarget.valueAsNumber / 100))}
          />
          <span class="unit">Chat</span>
        </div>
      </section>

      <section>
        <div class="head"><h4>Apps</h4></div>
        {#if sonar.apps.length === 0}
          <p class="hint">No app is playing audio right now.</p>
        {:else}
          <ul class="apps">
            {#each sonar.apps as app (app.id)}
              <li>
                <span class="app">{app.name}</span>
                <select
                  value={app.channel}
                  disabled={!app.binary || busy !== null}
                  onchange={(e) =>
                    run("route", () => daemon.setAppChannel(app.binary!, e.currentTarget.value as Channel))}
                >
                  {#each CHANNELS as c (c.value)}<option value={c.value}>{c.label}</option>{/each}
                </select>
              </li>
            {/each}
          </ul>
          <p class="hint">A change applies now and is remembered for that app.</p>
        {/if}
      </section>

      <section>
        <div class="head">
          <h4>Microphone</h4>
          {#if sonar.mic.present}<span class="unit wide">gglinux Mic is your default input</span>{/if}
        </div>
        {#if !sonar.mic.available}
          <p class="hint">
            Install <code>noise-suppression-for-voice</code> and <code>swh-plugins</code> for noise
            suppression, a gate and a compressor on your mic, then turn Sonar off and on.
          </p>
        {:else}
          {@const m = sonar.mic.settings}
          <div class="row">
            <span class="label">Input</span>
            <select
              value={sonar.mic.input ?? ""}
              onchange={(e) => run("mic-input", () => daemon.setMicInput(e.currentTarget.value))}
            >
              {#if !sonar.mic.input}<option value="">chosen when Sonar turns on</option>{/if}
              {#each sonar.mic.inputs as i (i.name)}<option value={i.name}>{i.description}</option>{/each}
            </select>
          </div>
          <div class="row">
            <span class="label">Volume</span>
            <input
              type="range"
              min="0"
              max="100"
              value={Math.round((sonar.mic.volume ?? 1) * 100)}
              disabled={!sonar.mic.present}
              onchange={(e) => quietly(() => daemon.setMicVolume(e.currentTarget.valueAsNumber / 100))}
            />
            <span class="unit">{pct(sonar.mic.volume)}</span>
          </div>
          <label class="check">
            <input type="checkbox" checked={m.denoise} onchange={(e) => micSettings({ denoise: e.currentTarget.checked })} />
            Noise suppression
          </label>
          <div class="row">
            <span class="label">Strength</span>
            <input
              type="range"
              min="0"
              max="95"
              step="5"
              value={m.denoise_strength}
              disabled={!m.denoise}
              onchange={(e) => micSettings({ denoise_strength: e.currentTarget.valueAsNumber })}
            />
            <span class="unit">{m.denoise_strength}%</span>
          </div>
          <label class="check">
            <input type="checkbox" checked={m.gate} onchange={(e) => micSettings({ gate: e.currentTarget.checked })} />
            Noise gate
          </label>
          <div class="row">
            <span class="label">Opens at</span>
            <input
              type="range"
              min="-80"
              max="-10"
              step="1"
              value={m.gate_threshold_db}
              disabled={!m.gate}
              onchange={(e) => micSettings({ gate_threshold_db: e.currentTarget.valueAsNumber })}
            />
            <span class="unit">{m.gate_threshold_db} dB</span>
          </div>
          <label class="check">
            <input
              type="checkbox"
              checked={m.compressor}
              onchange={(e) => micSettings({ compressor: e.currentTarget.checked })}
            />
            Compressor
          </label>
          <div class="row">
            <span class="label">Makeup</span>
            <input
              type="range"
              min="0"
              max="18"
              step="1"
              value={m.makeup_db}
              disabled={!m.compressor}
              onchange={(e) => micSettings({ makeup_db: e.currentTarget.valueAsNumber })}
            />
            <span class="unit">+{m.makeup_db} dB</span>
          </div>
        {/if}
      </section>

      <section>
        <div class="head">
          <h4>Equalizer</h4>
          <button class="ghost" onclick={() => setBands(flatten(tabBands))}>Reset gains</button>
        </div>
        <div class="tabs">
          {#each [...CHANNELS, { value: "headphone" as const, label: "Headphone" }] as t (t.value)}
            <button class:on={eqTab === t.value} onclick={() => (eqTab = t.value)}>{t.label}</button>
          {/each}
        </div>
        <EqEditor
          bands={tabBands}
          gainLimit={eqTab === "headphone" ? 20 : 12}
          preamp={eqTab === "headphone" ? sonar.headphone.preamp : 0}
          onchange={setBands}
        />
        {#if eqTab === "headphone"}
          <div class="row">
            <span class="label">Preamp</span>
            <input
              type="range"
              min="-20"
              max="20"
              step="0.5"
              value={sonar.headphone.preamp}
              onchange={(e) =>
                quietly(() =>
                  daemon.setHeadphoneEq({ ...sonar!.headphone, preamp: e.currentTarget.valueAsNumber }),
                )}
            />
            <span class="unit">{sonar.headphone.preamp} dB</span>
          </div>
          <p class="hint">
            Correction for {sonar.headphone.name ?? "your headphones"}, applied to every channel.
          </p>
          <textarea
            class="autoeq"
            rows="5"
            placeholder="Paste an AutoEQ ParametricEQ.txt (Preamp: … / Filter 1: ON PK Fc … )"
            bind:value={importText}
          ></textarea>
          <div class="row">
            <input class="name" placeholder="Headphone name (optional)" bind:value={importName} />
            <button class="apply" disabled={busy !== null || !importText.trim()} onclick={importAutoEq}>
              {busy === "import" ? "Importing…" : "Import AutoEQ"}
            </button>
          </div>
        {:else}
          <p class="hint">Drag a point to move it; scroll over it to change its width (Q).</p>
        {/if}
      </section>
    {/if}
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
    align-items: center;
    justify-content: space-between;
    gap: 1rem;
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
  .row {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    margin-top: 0.5rem;
  }
  .label {
    width: 4.5rem;
    font-size: 0.82rem;
    color: var(--text-dim);
  }
  .row input[type="range"] {
    flex: 1;
    max-width: 18rem;
  }
  .unit {
    width: 3.5rem;
    font-size: 0.78rem;
    color: var(--text-dim);
  }
  select {
    padding: 0.3rem 0.45rem;
    font-size: 0.85rem;
    color: var(--text);
    background: var(--surface-2);
    border: 1px solid var(--line);
    border-radius: 7px;
  }
  .apps {
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .apps li {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 0.6rem;
    max-width: 24rem;
    padding: 0.2rem 0;
  }
  .app {
    font-size: 0.85rem;
  }
  .hint {
    margin: 0.6rem 0 0;
    font-size: 0.76rem;
    color: var(--text-dim);
  }
  .hint.warn {
    color: var(--danger);
  }
  .apply {
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
  .tabs {
    display: inline-flex;
    flex-wrap: wrap;
    margin-bottom: 0.6rem;
    background: var(--surface-2);
    border: 1px solid var(--line);
    border-radius: 8px;
    overflow: hidden;
  }
  .tabs button {
    padding: 0.35rem 0.75rem;
    font-size: 0.82rem;
    color: var(--text-dim);
    background: none;
    border: none;
    cursor: pointer;
  }
  .tabs button.on {
    color: var(--bg);
    background: var(--accent);
    font-weight: 600;
  }
  .autoeq {
    width: 100%;
    max-width: 44rem;
    margin-top: 0.6rem;
    padding: 0.45rem 0.55rem;
    font-family: var(--mono);
    font-size: 0.78rem;
    color: var(--text);
    background: var(--surface-2);
    border: 1px solid var(--line);
    border-radius: 7px;
    resize: vertical;
  }
  .name {
    flex: 1;
    max-width: 20rem;
    padding: 0.3rem 0.45rem;
    font-size: 0.85rem;
    color: var(--text);
    background: var(--surface-2);
    border: 1px solid var(--line);
    border-radius: 7px;
  }
  .check {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    margin-top: 0.8rem;
    font-size: 0.85rem;
    cursor: pointer;
  }
  .check.inline {
    margin-top: 0;
  }
  .unit.wide {
    width: auto;
  }
  code {
    font-family: var(--mono);
    font-size: 0.78rem;
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
