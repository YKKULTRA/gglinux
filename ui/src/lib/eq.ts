// SPDX-License-Identifier: GPL-3.0-or-later
// Frequency response of an EQ chain, computed exactly as PipeWire's
// filter-chain filters are (spa/plugins/audioconvert/biquad.c): RBJ cookbook
// biquads with alpha = sin(w0) / 2Q for peaking bands and both shelves. What
// the editor draws is what PipeWire applies.

import type { EqBand } from "./types";

/** PipeWire's graph rate here; at 44.1 kHz the curve differs only near 20 kHz. */
const RATE = 48000;

interface Coeffs {
  b0: number;
  b1: number;
  b2: number;
  a0: number;
  a1: number;
  a2: number;
}

function coeffs(b: EqBand): Coeffs {
  const A = 10 ** (b.gain / 40);
  const w0 = (2 * Math.PI * b.freq) / RATE;
  const k = Math.cos(w0);
  const alpha = Math.sin(w0) / (2 * b.q);
  if (b.kind === "peaking") {
    return { b0: 1 + alpha * A, b1: -2 * k, b2: 1 - alpha * A, a0: 1 + alpha / A, a1: -2 * k, a2: 1 - alpha / A };
  }
  const k2 = 2 * Math.sqrt(A) * alpha;
  if (b.kind === "low_shelf") {
    return {
      b0: A * (A + 1 - (A - 1) * k + k2),
      b1: 2 * A * (A - 1 - (A + 1) * k),
      b2: A * (A + 1 - (A - 1) * k - k2),
      a0: A + 1 + (A - 1) * k + k2,
      a1: -2 * (A - 1 + (A + 1) * k),
      a2: A + 1 + (A - 1) * k - k2,
    };
  }
  return {
    b0: A * (A + 1 + (A - 1) * k + k2),
    b1: -2 * A * (A - 1 + (A + 1) * k),
    b2: A * (A + 1 + (A - 1) * k - k2),
    a0: A + 1 - (A - 1) * k + k2,
    a1: 2 * (A - 1 - (A + 1) * k),
    a2: A + 1 - (A - 1) * k - k2,
  };
}

/** |H(e^jw)| in dB at frequency f. */
function magnitudeDb(c: Coeffs, f: number): number {
  const w = (2 * Math.PI * f) / RATE;
  const cw = Math.cos(w);
  const c2w = Math.cos(2 * w);
  const num = c.b0 ** 2 + c.b1 ** 2 + c.b2 ** 2 + 2 * (c.b0 * c.b1 + c.b1 * c.b2) * cw + 2 * c.b0 * c.b2 * c2w;
  const den = c.a0 ** 2 + c.a1 ** 2 + c.a2 ** 2 + 2 * (c.a0 * c.a1 + c.a1 * c.a2) * cw + 2 * c.a0 * c.a2 * c2w;
  return 10 * Math.log10(num / den);
}

/** Combined response of a chain of bands at frequency f, in dB. A 0 dB band is flat. */
export function responseDb(bands: EqBand[], f: number): number {
  return bands.reduce((sum, b) => sum + (b.gain === 0 ? 0 : magnitudeDb(coeffs(b), f)), 0);
}
