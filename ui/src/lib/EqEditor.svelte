<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<script lang="ts">
  // A parametric EQ curve with draggable bands. Drag a point for frequency and
  // gain; scroll over it for Q. Changes go out at most every SEND_EVERY_MS while
  // dragging, and once more on release.

  import { untrack } from "svelte";
  import { responseDb } from "./eq";
  import type { EqBand } from "./types";

  interface Props {
    bands: EqBand[];
    /** ± dB the bands may reach; also the plot's range. */
    gainLimit: number;
    /** dB added to the whole curve (the headphone preamp). */
    preamp?: number;
    disabled?: boolean;
    onchange: (bands: EqBand[]) => void;
  }
  let { bands, gainLimit, preamp = 0, disabled = false, onchange }: Props = $props();

  const W = 640;
  const H = 220;
  const PAD = 28;
  const F_MIN = 20;
  const F_MAX = 20000;
  const SEND_EVERY_MS = 150;
  const LOG_MIN = Math.log10(F_MIN);
  const LOG_SPAN = Math.log10(F_MAX) - LOG_MIN;

  const clamp = (v: number, lo: number, hi: number) => Math.min(hi, Math.max(lo, v));
  const x = (f: number) => PAD + ((Math.log10(f) - LOG_MIN) / LOG_SPAN) * (W - 2 * PAD);
  const freqAt = (px: number) => 10 ** (LOG_MIN + ((px - PAD) / (W - 2 * PAD)) * LOG_SPAN);
  const y = (db: number) => H / 2 - (db / gainLimit) * (H / 2 - PAD / 2);
  const dbAt = (py: number) => ((H / 2 - py) / (H / 2 - PAD / 2)) * gainLimit;
  const FREQS = Array.from({ length: 160 }, (_, i) => 10 ** (LOG_MIN + (i / 159) * LOG_SPAN));
  const signed = (v: number) => (v > 0 ? `+${v}` : `${v}`);

  // Working copy: follows the daemon's bands whenever new ones arrive, except
  // mid-drag. `dragging` is read untracked so that letting go does not re-copy
  // the still-old bands (the reply to the final send is in flight) and snap the
  // point back for a moment.
  let local = $state<EqBand[]>([]);
  let dragging = $state<number | null>(null);
  $effect(() => {
    const incoming = bands;
    if (untrack(() => dragging) === null) local = incoming.map((b) => ({ ...b }));
  });

  let path = $derived(
    FREQS.map((f, i) => {
      const db = clamp(responseDb(local, f) + preamp, -gainLimit, gainLimit);
      return `${i ? "L" : "M"}${x(f).toFixed(1)},${y(db).toFixed(1)}`;
    }).join(" "),
  );

  let svg: SVGSVGElement | undefined = $state();
  let lastSent = 0;

  function send(force: boolean) {
    const now = performance.now();
    if (!force && now - lastSent < SEND_EVERY_MS) return;
    lastSent = now;
    onchange(local.map((b) => ({ ...b })));
  }

  function down(i: number, e: PointerEvent) {
    if (disabled) return;
    dragging = i;
    (e.currentTarget as Element).setPointerCapture(e.pointerId);
  }

  function move(e: PointerEvent) {
    if (dragging === null || !svg) return;
    const r = svg.getBoundingClientRect();
    const px = ((e.clientX - r.left) / r.width) * W;
    const py = ((e.clientY - r.top) / r.height) * H;
    const b = local[dragging];
    b.freq = Math.round(clamp(freqAt(px), F_MIN, F_MAX));
    b.gain = Math.round(clamp(dbAt(py), -gainLimit, gainLimit) * 2) / 2;
    send(false);
  }

  function up() {
    if (dragging === null) return;
    dragging = null;
    send(true);
  }

  function wheel(i: number, e: WheelEvent) {
    if (disabled) return;
    e.preventDefault();
    const b = local[i];
    b.q = Math.round(clamp(b.q * (e.deltaY < 0 ? 1.1 : 1 / 1.1), 0.1, 10) * 100) / 100;
    send(true);
  }
</script>

<svg
  bind:this={svg}
  viewBox="0 0 {W} {H}"
  class="eq"
  class:disabled
  role="group"
  aria-label="Equalizer curve"
  onpointermove={move}
  onpointerup={up}
  onpointercancel={up}
>
  {#each [100, 1000, 10000] as f (f)}
    <line class="grid" x1={x(f)} x2={x(f)} y1={PAD / 2} y2={H - PAD / 2} />
    <text class="axis" x={x(f) + 3} y={H - 6}>{f >= 1000 ? `${f / 1000}k` : f}</text>
  {/each}
  {#each [-gainLimit / 2, 0, gainLimit / 2] as db (db)}
    <line class="grid" class:zero={db === 0} x1={PAD} x2={W - PAD} y1={y(db)} y2={y(db)} />
    <text class="axis" x="2" y={y(db) + 4}>{signed(db)}</text>
  {/each}
  <path d={path} class="curve" />
  {#each local as b, i (i)}
    <circle
      cx={x(b.freq)}
      cy={y(b.gain)}
      r={dragging === i ? 8 : 6}
      class="handle"
      class:shelf={b.kind !== "peaking"}
      role="slider"
      tabindex="-1"
      aria-label="Band {i + 1}"
      aria-valuenow={b.gain}
      aria-valuemin={-gainLimit}
      aria-valuemax={gainLimit}
      onpointerdown={(e) => down(i, e)}
      onwheel={(e) => wheel(i, e)}
    >
      <title>{Math.round(b.freq)} Hz · {signed(b.gain)} dB · Q {b.q}</title>
    </circle>
  {/each}
</svg>

<style>
  .eq {
    width: 100%;
    max-width: 44rem;
    height: auto;
    background: var(--surface-2);
    border: 1px solid var(--line);
    border-radius: 10px;
    touch-action: none;
    user-select: none;
  }
  .eq.disabled {
    opacity: 0.5;
  }
  .grid {
    stroke: var(--line);
    stroke-width: 1;
  }
  .grid.zero {
    stroke: var(--text-dim);
    stroke-dasharray: 3 3;
  }
  .axis {
    fill: var(--text-dim);
    font-size: 10px;
    font-family: var(--mono);
  }
  .curve {
    fill: none;
    stroke: var(--accent);
    stroke-width: 2;
  }
  .handle {
    fill: var(--surface);
    stroke: var(--accent);
    stroke-width: 2;
    cursor: grab;
  }
  .handle.shelf {
    stroke-dasharray: 2 2;
  }
  .handle:active {
    cursor: grabbing;
  }
</style>
