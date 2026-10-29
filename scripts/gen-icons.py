#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Render the gglinux app icon at every size the app and the packages install.

Draws once at 1024px and downsamples, so there is a single source of truth for the
mark. Run after changing the design:  python3 scripts/gen-icons.py
"""

from pathlib import Path

from PIL import Image, ImageDraw

S = 1024
OUT = Path(__file__).resolve().parent.parent / "data" / "icons"

BG_TOP = (28, 32, 39)
BG_BOTTOM = (12, 14, 17)
TRACK = (46, 52, 62)
ACCENT = (255, 122, 41)
ACCENT2 = (255, 77, 109)

# (vertical centre, knob position) as fractions of the canvas — a three-fader mixer.
FADERS = [(0.335, 0.70), (0.500, 0.38), (0.665, 0.80)]


def lerp(a, b, t):
    return tuple(round(x + (y - x) * t) for x, y in zip(a, b))


def render() -> Image.Image:
    img = Image.new("RGBA", (S, S), (0, 0, 0, 0))

    # Rounded-square body with a vertical gradient.
    column = Image.new("RGBA", (1, S))
    for y in range(S):
        column.putpixel((0, y), (*lerp(BG_TOP, BG_BOTTOM, y / (S - 1)), 255))
    body = column.resize((S, S))

    mask = Image.new("L", (S, S), 0)
    ImageDraw.Draw(mask).rounded_rectangle(
        [0, 0, S - 1, S - 1], radius=round(S * 0.225), fill=255
    )
    img.paste(body, (0, 0), mask)

    d = ImageDraw.Draw(img)
    x0, x1 = S * 0.205, S * 0.795
    half = S * 0.026
    knob_r = S * 0.062

    for i, (cy_frac, knob_frac) in enumerate(FADERS):
        cy = S * cy_frac
        colour = lerp(ACCENT, ACCENT2, i / (len(FADERS) - 1))
        knob_x = x0 + (x1 - x0) * knob_frac

        d.rounded_rectangle([x0, cy - half, x1, cy + half], radius=half, fill=TRACK)
        d.rounded_rectangle(
            [x0, cy - half, knob_x, cy + half], radius=half, fill=(*colour, 255)
        )
        d.ellipse(
            [knob_x - knob_r, cy - knob_r, knob_x + knob_r, cy + knob_r],
            fill=(245, 247, 250, 255),
        )

    return img


def main() -> None:
    OUT.mkdir(parents=True, exist_ok=True)
    master = render()
    for name, size in [
        ("32x32.png", 32),
        ("128x128.png", 128),
        ("128x128@2x.png", 256),
        ("icon.png", 512),
    ]:
        master.resize((size, size), Image.LANCZOS).save(OUT / name)
        print(f"wrote {OUT / name} ({size}px)")


if __name__ == "__main__":
    main()
