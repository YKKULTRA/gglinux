<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<script lang="ts">
  // The Apex's 128 × 40 screen: the keyboard's own menus, text, a clock, or a picture.
  // gglinux draws it while the daemon runs and hands it back when it stops.

  import { daemon } from "./daemon.svelte";
  import type { KeyboardScreen, KeyboardState } from "./types";

  let { kb }: { kb: KeyboardState } = $props();

  const W = 128;
  const H = 40;
  type Kind = KeyboardScreen["kind"];
  const KINDS: { kind: Kind; label: string }[] = [
    { kind: "keyboard", label: "Keyboard's own" },
    { kind: "text", label: "Text" },
    { kind: "clock", label: "Clock" },
    { kind: "image", label: "Picture" },
  ];

  let screen = $derived(kb.screen);
  let busy = $state(false);
  let error = $state<string | null>(null);
  /** The tab picked but not sent yet (text and pictures wait for "Show"). */
  let picked = $state<Kind | null>(null);
  let kind = $derived(picked ?? screen.kind);
  let text = $state("");
  let clock = $derived(
    screen.kind === "clock" ? screen : { kind: "clock" as const, seconds: false, h24: true, date: true },
  );

  let source = $state<HTMLImageElement | null>(null);
  let threshold = $state(128);
  let invert = $state(false);
  let dither = $state(true);
  let pixels = $state<number[] | null>(null);
  let preview = $state<HTMLCanvasElement | undefined>();
  let filePicker = $state<HTMLInputElement | undefined>();
  let fileName = $state<string | null>(null);

  $effect(() => {
    if (screen.kind === "text") text = screen.text;
  });

  $effect(() => {
    if (source) pixels = convert(source, threshold, invert, dither);
  });

  $effect(() => {
    const bits = pixels ?? (screen.kind === "image" ? screen.pixels : null);
    if (preview && bits) draw(preview, bits);
  });

  let lines = $derived(text.split("\n"));
  let tooLong = $derived(lines.length > 5 || lines.some((l) => l.length > 16));

  async function apply(s: KeyboardScreen) {
    busy = true;
    error = null;
    try {
      await daemon.setScreen(s);
      picked = null;
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      busy = false;
    }
  }

  function choose(k: Kind) {
    picked = k;
    if (k === "keyboard") void apply({ kind: "keyboard" });
    else if (k === "clock") void apply(clock);
  }

  function load(file: File | undefined) {
    if (!file) return;
    fileName = file.name;
    const url = URL.createObjectURL(file);
    const img = new Image();
    img.onload = () => {
      source = img;
      URL.revokeObjectURL(url);
    };
    img.onerror = () => {
      error = "That file isn't a picture this app can read.";
      URL.revokeObjectURL(url);
    };
    img.src = url;
  }

  /** The picture fitted into 128 × 40 on black, one bit a pixel, 16 bytes a row, the
   *  leftmost pixel in each byte's high bit. Floyd–Steinberg dithering keeps greys. */
  function convert(img: HTMLImageElement, threshold: number, invert: boolean, dither: boolean): number[] {
    const c = document.createElement("canvas");
    c.width = W;
    c.height = H;
    const g = c.getContext("2d");
    if (!g) return new Array<number>(640).fill(0);
    g.fillStyle = "#000";
    g.fillRect(0, 0, W, H);
    const scale = Math.min(W / img.naturalWidth, H / img.naturalHeight);
    const w = img.naturalWidth * scale;
    const h = img.naturalHeight * scale;
    g.drawImage(img, (W - w) / 2, (H - h) / 2, w, h);
    const data = g.getImageData(0, 0, W, H).data;
    const lum = new Float32Array(W * H);
    for (let i = 0; i < W * H; i++) lum[i] = 0.299 * data[i * 4] + 0.587 * data[i * 4 + 1] + 0.114 * data[i * 4 + 2];
    const bits = new Array<number>(640).fill(0);
    for (let y = 0; y < H; y++) {
      for (let x = 0; x < W; x++) {
        const i = y * W + x;
        const on = lum[i] >= threshold;
        if (dither) {
          const err = lum[i] - (on ? 255 : 0);
          if (x + 1 < W) lum[i + 1] += (err * 7) / 16;
          if (y + 1 < H) {
            if (x > 0) lum[i + W - 1] += (err * 3) / 16;
            lum[i + W] += (err * 5) / 16;
            if (x + 1 < W) lum[i + W + 1] += err / 16;
          }
        }
        if (on !== invert) bits[y * 16 + (x >> 3)] |= 0x80 >> (x & 7);
      }
    }
    return bits;
  }

  function draw(canvas: HTMLCanvasElement, bits: number[]) {
    const g = canvas.getContext("2d");
    if (!g) return;
    const img = g.createImageData(W, H);
    for (let y = 0; y < H; y++) {
      for (let x = 0; x < W; x++) {
        const on = (bits[y * 16 + (x >> 3)] & (0x80 >> (x & 7))) !== 0;
        const p = (y * W + x) * 4;
        img.data[p] = img.data[p + 1] = img.data[p + 2] = on ? 235 : 10;
        img.data[p + 3] = 255;
      }
    }
    g.putImageData(img, 0, 0);
  }
</script>

<section>
  <div class="head"><h4>Screen</h4></div>
  <div class="segmented">
    {#each KINDS as k (k.kind)}
      <button class:on={kind === k.kind} disabled={busy} onclick={() => choose(k.kind)}>{k.label}</button>
    {/each}
  </div>

  {#if kind === "text"}
    <div class="row">
      <textarea rows="5" cols="17" spellcheck="false" bind:value={text} disabled={busy}></textarea>
    </div>
    <p class="hint" class:warn={tooLong}>Up to 5 lines of 16 characters, plain ASCII. Short text is drawn bigger.</p>
    <button class="apply" disabled={busy || tooLong || text.trim() === ""} onclick={() => apply({ kind: "text", text })}>
      Show on the keyboard
    </button>
  {:else if kind === "clock"}
    <div class="row">
      <label class="toggle">
        <input
          type="checkbox"
          checked={clock.h24}
          disabled={busy}
          onchange={(e) => apply({ ...clock, h24: e.currentTarget.checked })}
        />
        24-hour
      </label>
      <label class="toggle">
        <input
          type="checkbox"
          checked={clock.seconds}
          disabled={busy}
          onchange={(e) => apply({ ...clock, seconds: e.currentTarget.checked })}
        />
        Seconds
      </label>
      <label class="toggle">
        <input
          type="checkbox"
          checked={clock.date}
          disabled={busy}
          onchange={(e) => apply({ ...clock, date: e.currentTarget.checked })}
        />
        Date
      </label>
    </div>
  {:else if kind === "image"}
    <div class="row">
      <button class="ghost" disabled={busy} onclick={() => filePicker?.click()}>Choose a picture…</button>
      {#if fileName}<span class="meta">{fileName}</span>{/if}
      <input
        bind:this={filePicker}
        type="file"
        accept="image/*"
        hidden
        onchange={(e) => load(e.currentTarget.files?.[0])}
      />
    </div>
    <canvas class="preview" width={W} height={H} bind:this={preview}></canvas>
    {#if source}
      <div class="row">
        <span class="label">Threshold</span>
        <input type="range" min="1" max="254" bind:value={threshold} />
        <label class="toggle"><input type="checkbox" bind:checked={dither} /> Dither</label>
        <label class="toggle"><input type="checkbox" bind:checked={invert} /> Invert</label>
      </div>
      <button class="apply" disabled={busy || !pixels} onclick={() => pixels && apply({ kind: "image", pixels })}>
        Show on the keyboard
      </button>
    {/if}
  {/if}

  {#if kb.screen_error}
    <p class="error">The keyboard refused the picture: {kb.screen_error}</p>
  {/if}
  {#if error}
    <p class="error">{error}</p>
  {/if}
</section>

<style>
  section { margin-top: 1.1rem; padding-top: 1rem; border-top: 1px solid var(--line); }
  .head { display: flex; align-items: center; justify-content: space-between; gap: 0.6rem; margin-bottom: 0.6rem; }
  h4 { margin: 0; font-size: 0.8rem; font-weight: 600; text-transform: uppercase; letter-spacing: 0.05em; color: var(--text-dim); }
  .segmented { display: inline-flex; flex-wrap: wrap; background: var(--surface-2); border: 1px solid var(--line); border-radius: 8px; overflow: hidden; }
  .segmented button { padding: 0.35rem 0.75rem; font-size: 0.82rem; color: var(--text-dim); background: none; border: none; cursor: pointer; }
  .segmented button.on { color: var(--bg); background: var(--accent); font-weight: 600; }
  .toggle { display: flex; align-items: center; gap: 0.5rem; font-size: 0.85rem; cursor: pointer; }
  .row { display: flex; align-items: center; gap: 0.8rem; margin-top: 0.7rem; flex-wrap: wrap; }
  .label { width: 5.5rem; font-size: 0.82rem; color: var(--text-dim); }
  .hint { margin: 0.5rem 0 0; font-size: 0.78rem; color: var(--text-dim); }
  .meta { font-size: 0.8rem; color: var(--text-dim); }
  .ghost { padding: 0.25rem 0.55rem; font-size: 0.78rem; color: var(--text-dim); background: none; border: 1px solid var(--line); border-radius: 7px; cursor: pointer; }
  .ghost:hover:not(:disabled) { border-color: var(--accent); color: var(--accent); }
  .ghost:disabled { opacity: 0.4; cursor: default; }
  .hint.warn { color: var(--danger); }
  textarea { font-family: ui-monospace, monospace; font-size: 0.9rem; color: var(--text); background: var(--surface-2); border: 1px solid var(--line); border-radius: 8px; padding: 0.4rem 0.5rem; resize: none; }
  .preview { display: block; width: 384px; height: 120px; margin-top: 0.7rem; image-rendering: pixelated; border: 1px solid var(--line); border-radius: 6px; background: #0a0a0a; }
  .apply { margin-top: 0.75rem; padding: 0.4rem 0.85rem; font-size: 0.83rem; font-weight: 500; color: var(--text); background: var(--surface-2); border: 1px solid var(--line); border-radius: 8px; cursor: pointer; }
  .apply:hover:not(:disabled) { border-color: var(--accent); color: var(--accent); }
  .apply:disabled { opacity: 0.5; cursor: default; }
  .error { margin: 0.8rem 0 0; padding: 0.5rem 0.7rem; font-size: 0.8rem; color: var(--danger); background: color-mix(in srgb, var(--danger) 12%, transparent); border-radius: 8px; }
</style>
