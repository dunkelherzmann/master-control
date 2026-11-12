#!/usr/bin/env python3
"""Draws the Master Control icon candidates as SVG. Every shape here is our own drawing: a brain of two lobed halves
whose folds are circuit traces ending in coloured nodes, resting on a chip, inside a circle on a rounded square."""
import pathlib

BLUE, GREEN, ORANGE, PURPLE, PINK, CYAN, YELLOW = "#0A84FF", "#30D158", "#FF9F0A", "#BF5AF2", "#FF375F", "#64D2FF", "#FFD60A"
DARK, DARK2, GREY = "#1C1C1E", "#2C2C2E", "#3A3A3C"

# One half of the brain, drawn for the left side around the centre line x=0; the right is its mirror image.
HALF = "M-9,-96 C-40,-110 -74,-96 -82,-68 C-110,-62 -124,-34 -112,-8 C-128,16 -120,48 -96,58 C-92,82 -64,96 -40,86 C-30,98 -14,94 -9,82 Z"
# Folds as traces: (path, node x, node y)
TRACES = [("M-9,-52 H-40 V-74", -40, -74), ("M-9,-6 H-50 L-74,-28", -74, -28), ("M-9,40 H-44 V62", -44, 62), ("M-58,10 H-92", -92, 10)]
# Wires from the chip out to the circle's edge: (path, node x, node y)
WIRES = [("M-34,112 H-78 L-104,138", -104, 138), ("M-34,136 H-66 V168", -66, 168), ("M0,158 V186", 0, 186)]


def brain(stroke, fill, nodes_left, nodes_right, chip_fill, chip_text, width=9, text="MC"):
    parts = []
    for side, colours in ((1, nodes_left), (-1, nodes_right)):
        g = [f'<g transform="scale({side},1)">', f'<path d="{HALF}" fill="{fill}" stroke="{stroke}" stroke-width="{width}" stroke-linejoin="round"/>']
        for (path, x, y), colour in zip(TRACES, colours):
            g.append(f'<path d="{path}" fill="none" stroke="{stroke}" stroke-width="{width - 2}" stroke-linecap="round" stroke-linejoin="round"/>')
            g.append(f'<circle cx="{x}" cy="{y}" r="10" fill="{colour}" stroke="{stroke}" stroke-width="4"/>')
        for (path, x, y), colour in zip(WIRES[:2], colours[2:] + colours[:2]):
            g.append(f'<path d="{path}" fill="none" stroke="{stroke}" stroke-width="{width - 2}" stroke-linecap="round" stroke-linejoin="round"/>')
            g.append(f'<circle cx="{x}" cy="{y}" r="10" fill="{colour}" stroke="{stroke}" stroke-width="4"/>')
        g.append("</g>")
        parts += g
    path, x, y = WIRES[2]
    parts.append(f'<path d="{path}" fill="none" stroke="{stroke}" stroke-width="{width - 2}" stroke-linecap="round"/>')
    parts.append(f'<circle cx="{x}" cy="{y}" r="10" fill="{YELLOW}" stroke="{stroke}" stroke-width="4"/>')
    parts.append(f'<rect x="-40" y="92" width="80" height="66" rx="14" fill="{chip_fill}" stroke="{stroke}" stroke-width="{width}"/>')
    parts.append(f'<text x="0" y="138" font-family="SF Pro Rounded, SF Pro Display, Helvetica Neue, Arial, sans-serif" font-weight="800" font-size="34" text-anchor="middle" fill="{chip_text}">{text}</text>')
    return "\n".join(parts)


def icon(name, background, circle, glyph, extra=""):
    svg = f'''<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 512 512" width="512" height="512">
<defs>
  <linearGradient id="bg" x1="0" y1="0" x2="0" y2="1">{background}</linearGradient>
  <linearGradient id="disc" x1="0" y1="0" x2="0" y2="1">{circle}</linearGradient>
  <linearGradient id="blue" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#3FA2FF"/><stop offset="1" stop-color="#0A6CF0"/></linearGradient>
</defs>
<rect x="28" y="28" width="456" height="456" rx="104" fill="url(#bg)"/>
<rect x="28.5" y="28.5" width="455" height="455" rx="103.5" fill="none" stroke="#FFFFFF" stroke-opacity="0.10"/>
{extra}
<g transform="translate(256,216) scale(0.98)">{glyph}</g>
</svg>
'''
    pathlib.Path(f"{name}.svg").write_text(svg)


def stops(top, bottom):
    return f'<stop offset="0" stop-color="{top}"/><stop offset="1" stop-color="{bottom}"/>'


def disc(fill, r=186, stroke=None):
    ring = f' stroke="{stroke}" stroke-width="6"' if stroke else ""
    return f'<circle cx="256" cy="256" r="{r}" fill="{fill}"{ring}/>'


LEFT, RIGHT = [BLUE, GREEN, ORANGE, CYAN], [PURPLE, PINK, YELLOW, GREEN]

# A: the dark Apps square, a blue disc, the brain in white line with a dark chip.
icon("a-dark-blue-disc", stops("#2E2E31", DARK), "", brain("#FFFFFF", "#FFFFFF22", LEFT, RIGHT, DARK, "#FFFFFF"), disc("url(#blue)"))
# B: a blue square as in the first reference's shape, a dark disc, coloured nodes glowing on it.
icon("b-blue-dark-disc", stops("#3FA2FF", "#0A6CF0"), "", brain("#F2F2F7", "#3A3A3C", LEFT, RIGHT, "#F2F2F7", DARK), disc(DARK))
# C: all dark, the disc a shade lighter with a thin ring, the brain filled soft white.
icon("c-dark-soft-disc", stops("#2A2A2D", "#161618"), "", brain(DARK, "#F2F2F7", LEFT, RIGHT, BLUE, "#FFFFFF"), disc(GREY, stroke="#545458"))
# D: dark square, and the circle is a ring made of the six colours; the brain in white line inside.
ring = "".join(
    f'<circle cx="256" cy="256" r="180" fill="none" stroke="{c}" stroke-width="16" stroke-linecap="round" stroke-dasharray="150 981" transform="rotate({-90 + i * 60 + 8} 256 256)"/>'
    for i, c in enumerate([BLUE, GREEN, YELLOW, ORANGE, PINK, PURPLE])
)
icon("d-dark-colour-ring", stops("#2E2E31", DARK), "", brain("#F2F2F7", "#FFFFFF14", [BLUE, GREEN, YELLOW, CYAN], [PURPLE, PINK, ORANGE, GREEN], BLUE, "#FFFFFF"), ring)
# E: a white disc on the dark square, the brain drawn dark, for the strongest contrast at small sizes.
icon("e-dark-white-disc", stops("#2E2E31", DARK), "", brain(DARK, "#E5E5EA", LEFT, RIGHT, DARK, "#FFFFFF"), disc("#F2F2F7"))
# F: the blue disc again, but each half of the brain filled with colour: a livelier take.
half_fill = brain("#FFFFFF", "#FFFFFF", LEFT, RIGHT, DARK, "#FFFFFF")
icon("f-dark-blue-disc-solid", stops("#2E2E31", DARK), "", brain(DARK, "#FFFFFF", LEFT, RIGHT, DARK, "#FFFFFF"), disc("url(#blue)"))

names = sorted(p.stem for p in pathlib.Path(".").glob("[a-f]-*.svg"))
cells = "".join(
    f'<g transform="translate({40 + (i % 3) * 560},{40 + (i // 3) * 620})"><image href="{n}.png" width="512" height="512"/>'
    f'<text x="256" y="560" font-family="Helvetica Neue, Arial, sans-serif" font-size="30" text-anchor="middle" fill="#1C1C1E">{n[0].upper()}</text></g>'
    for i, n in enumerate(names)
)
pathlib.Path("sheet.svg").write_text(f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1720 1280" width="1720" height="1280"><rect width="1720" height="1280" fill="#F2F2F7"/>{cells}</svg>')
print(names)
