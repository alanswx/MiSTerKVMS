"""MiSTerKVMS app icon: MiSTer-Kun on a small monitor with keyboard and mouse.

    python3 apple/Icon/make_icon.py            # writes AppIcon.icns + AppIcon.png
    python3 apple/Icon/make_icon.py --all      # also renders the other variants

Needs rsvg-convert (brew install librsvg) and iconutil (Xcode tools).
MiSTer-Kun artwork: see CREDITS.md in this folder.
"""
import re
import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).parent
KUN = (HERE / "misterkun.svg").read_text()
# Inner drawing of the 1000x1000 mascot (drop the XML/svg wrapper).
KUN_BODY = re.search(r"<svg[^>]*>(.*)</svg>", KUN, re.S).group(1)


def kun(cx, cy, size):
    s = size / 1000
    return f'<g transform="translate({cx - size / 2:.1f},{cy - size / 2:.1f}) scale({s:.4f})">{KUN_BODY}</g>'


def keyboard(x, y, w, h, body, edge, key, key_edge):
    out = [
        f'<rect x="{x}" y="{y + 10}" width="{w}" height="{h}" rx="20" fill="{edge}"/>',
        f'<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="20" fill="{body}"/>',
    ]
    rows = 4
    pad = 18
    kh = (h - pad * 2 - (rows - 1) * 7) / rows
    for r in range(rows):
        ky = y + pad + r * (kh + 7)
        if r == rows - 1:
            # bottom row: short, space bar, short
            widths = [0.16, 0.56, 0.16]
        else:
            widths = [1 / 11] * 11
        gap = 7
        total = w - pad * 2 - gap * (len(widths) - 1)
        kx = x + pad + (r % 2) * 6 if r < rows - 1 else x + pad
        if r < rows - 1:
            total -= (r % 2) * 6
        for fw in widths:
            kw = total * fw / sum(widths)
            out.append(f'<rect x="{kx:.1f}" y="{ky + 3:.1f}" width="{kw:.1f}" height="{kh:.1f}" rx="5" fill="{key_edge}"/>')
            out.append(f'<rect x="{kx:.1f}" y="{ky:.1f}" width="{kw:.1f}" height="{kh:.1f}" rx="5" fill="{key}"/>')
            kx += kw + gap
    return "\n".join(out)


def mouse(x, y, body, edge, line, cord, cord_to):
    w, h = 74, 104
    return f"""
<path d="M{x + w / 2},{y} C{x + w / 2},{y - 40} {cord_to[0] + 40},{cord_to[1] - 30} {cord_to[0]},{cord_to[1]}"
      fill="none" stroke="{cord}" stroke-width="7" stroke-linecap="round"/>
<rect x="{x}" y="{y + 8}" width="{w}" height="{h}" rx="34" fill="{edge}"/>
<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="34" fill="{body}"/>
<path d="M{x + 4},{y + 40} H{x + w - 4} M{x + w / 2},{y + 2} V{y + 40}" stroke="{line}" stroke-width="4" fill="none"/>
"""


def icon(bg, body, body_edge, bezel, screen_a, screen_b, kb_body, kb_edge, key, key_edge,
         led, cord, vents=True):
    mon = dict(x=236, y=176, w=552, h=468)
    sc = dict(x=290, y=226, w=444, h=330)
    return f"""<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024" viewBox="0 0 1024 1024">
<defs>
  <linearGradient id="bg" x1="0" y1="0" x2="0" y2="1">
    <stop offset="0" stop-color="{bg[0]}"/><stop offset="1" stop-color="{bg[1]}"/>
  </linearGradient>
  <linearGradient id="body" x1="0" y1="0" x2="0" y2="1">
    <stop offset="0" stop-color="{body[0]}"/><stop offset="1" stop-color="{body[1]}"/>
  </linearGradient>
  <radialGradient id="screen" cx="0.5" cy="0.45" r="0.75">
    <stop offset="0" stop-color="{screen_a}"/><stop offset="1" stop-color="{screen_b}"/>
  </radialGradient>
  <linearGradient id="glare" x1="0" y1="0" x2="1" y2="1">
    <stop offset="0" stop-color="#fff" stop-opacity="0.22"/><stop offset="0.45" stop-color="#fff" stop-opacity="0"/>
  </linearGradient>
  <filter id="shadow" x="-20%" y="-20%" width="140%" height="150%">
    <feGaussianBlur in="SourceAlpha" stdDeviation="14"/>
    <feOffset dy="14" result="b"/>
    <feComponentTransfer><feFuncA type="linear" slope="0.35"/></feComponentTransfer>
    <feMerge><feMergeNode/><feMergeNode in="SourceGraphic"/></feMerge>
  </filter>
  <clipPath id="screenclip"><rect x="{sc['x']}" y="{sc['y']}" width="{sc['w']}" height="{sc['h']}" rx="34"/></clipPath>
</defs>

<!-- macOS icon shape -->
<rect x="100" y="100" width="824" height="824" rx="185" fill="url(#bg)"/>

<g filter="url(#shadow)">
  <!-- stand -->
  <path d="M430,640 H594 L626,690 H398 Z" fill="{body_edge}"/>
  <rect x="384" y="682" width="256" height="22" rx="10" fill="{body_edge}"/>
  <!-- monitor -->
  <rect x="{mon['x']}" y="{mon['y'] + 12}" width="{mon['w']}" height="{mon['h']}" rx="52" fill="{body_edge}"/>
  <rect x="{mon['x']}" y="{mon['y']}" width="{mon['w']}" height="{mon['h']}" rx="52" fill="url(#body)"/>
  <rect x="{sc['x'] - 18}" y="{sc['y'] - 18}" width="{sc['w'] + 36}" height="{sc['h'] + 36}" rx="44" fill="{bezel}"/>
  <rect x="{sc['x']}" y="{sc['y']}" width="{sc['w']}" height="{sc['h']}" rx="34" fill="url(#screen)"/>
  <g clip-path="url(#screenclip)">
    {kun(512, 396, 262)}
    <rect x="{sc['x']}" y="{sc['y']}" width="{sc['w']}" height="{sc['h']}" fill="url(#glare)"/>
  </g>
  {"".join(f'<rect x="{300 + i * 22}" y="604" width="12" height="18" rx="4" fill="{body_edge}"/>' for i in range(6)) if vents else ""}
  <circle cx="720" cy="613" r="9" fill="{led}"/>

  <!-- keyboard and mouse -->
  {keyboard(268, 724, 400, 118, kb_body, kb_edge, key, key_edge)}
  {mouse(706, 742, kb_body, kb_edge, kb_edge, cord, (668, 760))}
</g>
</svg>
"""


VARIANTS = {
    "A-beige-blue": dict(
        bg=("#3a6fb0", "#1d2f63"), body=("#f3ead6", "#ddd0b3"), body_edge="#b8a985",
        bezel="#5b5446", screen_a="#2c5a49", screen_b="#0c1a14",
        kb_body="#efe5cf", kb_edge="#b8a985", key="#fbf6ea", key_edge="#cbbd9c",
        led="#5cf07a", cord="#8f8366"),
    "B-graphite-light": dict(
        bg=("#f4f6f9", "#c9d3e3"), body=("#4a4f58", "#2c3037"), body_edge="#1b1e23",
        bezel="#111316", screen_a="#4fb3ff", screen_b="#1557b0",
        kb_body="#3a3e46", kb_edge="#1b1e23", key="#5b616c", key_edge="#23262c",
        led="#4cd964", cord="#2c3037", vents=False),
    "C-beige-warm": dict(
        bg=("#ff7a45", "#c2185b"), body=("#f3ead6", "#ddd0b3"), body_edge="#b8a985",
        bezel="#5b5446", screen_a="#365e8f", screen_b="#0f1d33",
        kb_body="#efe5cf", kb_edge="#b8a985", key="#fbf6ea", key_edge="#cbbd9c",
        led="#5cf07a", cord="#8f8366"),
}

# The shipped icon. A (dark) and B (light) are kept for a possible
# appearance-aware icon later.
CHOSEN = "C-beige-warm"


def render(svg: Path, png: Path, size: int):
    subprocess.run(["rsvg-convert", "-w", str(size), "-h", str(size), str(svg), "-o", str(png)], check=True)


if __name__ == "__main__":
    names = VARIANTS if "--all" in sys.argv else [CHOSEN]
    with tempfile.TemporaryDirectory() as tmp:
        tmp = Path(tmp)
        for name in names:
            svg = tmp / f"{name}.svg"
            svg.write_text(icon(**VARIANTS[name]))
            if name != CHOSEN:
                render(svg, HERE / f"variant-{name}.png", 1024)
                continue
            render(svg, HERE / "AppIcon.png", 1024)
            iconset = tmp / "AppIcon.iconset"
            iconset.mkdir()
            for pt in (16, 32, 128, 256, 512):
                render(svg, iconset / f"icon_{pt}x{pt}.png", pt)
                render(svg, iconset / f"icon_{pt}x{pt}@2x.png", pt * 2)
            subprocess.run(["iconutil", "-c", "icns", str(iconset), "-o", str(HERE / "AppIcon.icns")], check=True)
    print("Wrote AppIcon.icns and AppIcon.png")
