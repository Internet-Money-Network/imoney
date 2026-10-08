"""Draws the Internet Money mark and banner as SVG and PNG.

    python assets/branding/render.py

Needs Pillow. The mark is three bars and a dot in a note frame; the two colourings are for
light backgrounds (green square) and for the green band at the head of a page (paper square).
"""
import math
import os
import shutil

from PIL import Image, ImageDraw, ImageFont

GREEN, PAPER, SOFT, LIVE, LIVE_DARK, MID = "#0f2e23", "#eef0e6", "#b9c9a4", "#6fe0a6", "#0b7a4b", "#3d6652"
HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))

# The mark on a 512 grid
BAR = (86, 128, 142, 384)
SLANTS = [
    [(182, 384), (182, 232), (242, 128), (298, 128), (238, 232), (238, 384)],
    [(310, 384), (310, 232), (370, 128), (426, 128), (366, 232), (366, 384)],
]
DOT = (114, 94, 16)
FRAME_INSET, FRAME_WIDTH = 30, 5


def mark_svg(inverse=False, body_only=False):
    field, ink, frame, dot = (PAPER, GREEN, MID, LIVE_DARK) if inverse else (GREEN, PAPER, SOFT, LIVE)
    size = 512 - 2 * FRAME_INSET
    parts = [
        f'<rect width="512" height="512" fill="{field}"/>',
        f'<rect x="{FRAME_INSET}" y="{FRAME_INSET}" width="{size}" height="{size}" fill="none" stroke="{frame}" stroke-width="{FRAME_WIDTH}"/>',
        f'<rect x="{BAR[0]}" y="{BAR[1]}" width="{BAR[2] - BAR[0]}" height="{BAR[3] - BAR[1]}" fill="{ink}"/>',
    ]
    for points in SLANTS:
        parts.append(f'<path d="M{" L".join(f"{x} {y}" for x, y in points)} Z" fill="{ink}"/>')
    parts.append(f'<circle cx="{DOT[0]}" cy="{DOT[1]}" r="{DOT[2]}" fill="{dot}"/>')
    body = "\n  ".join(parts)
    if body_only:
        return body
    return f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 512 512">\n  {body}\n</svg>\n'


def draw_mark(draw, x, y, size, inverse=False):
    field, ink, frame, dot = (PAPER, GREEN, MID, LIVE_DARK) if inverse else (GREEN, PAPER, SOFT, LIVE)
    k = size / 512
    at = lambda px, py: (x + px * k, y + py * k)
    draw.rectangle([at(0, 0), at(512, 512)], fill=field)
    draw.rectangle([at(FRAME_INSET, FRAME_INSET), at(512 - FRAME_INSET, 512 - FRAME_INSET)], outline=frame, width=max(1, round(FRAME_WIDTH * k)))
    draw.rectangle([at(BAR[0], BAR[1]), at(BAR[2], BAR[3])], fill=ink)
    for points in SLANTS:
        draw.polygon([at(px, py) for px, py in points], fill=ink)
    cx, cy, r = DOT
    draw.ellipse([at(cx - r, cy - r), at(cx + r, cy + r)], fill=dot)


def wave(width, mid, amplitude, period, flip=1):
    return [(px, mid + flip * amplitude * math.sin(2 * math.pi * px / period)) for px in range(0, width + 1, 2)]


def font(names, size):
    for name in names:
        try:
            return ImageFont.truetype(name, size)
        except OSError:
            continue
    return ImageFont.load_default(size)


def icon_png(path, size=512):
    scale = 4
    image = Image.new("RGB", (size * scale, size * scale), GREEN)
    draw_mark(ImageDraw.Draw(image), 0, 0, size * scale)
    image.resize((size, size), Image.LANCZOS).save(path)


def banner_png(path, width=1100, height=240):
    scale = 3
    image = Image.new("RGB", (width * scale, height * scale), GREEN)
    draw = ImageDraw.Draw(image)
    s = lambda v: v * scale
    draw_mark(draw, s(40), s(40), s(160), inverse=True)
    serif = font(["georgiab.ttf", "DejaVuSerif-Bold.ttf", "LiberationSerif-Bold.ttf"], s(62))
    mono = font(["consola.ttf", "DejaVuSansMono.ttf", "LiberationMono-Regular.ttf"], s(18))
    draw.text((s(240), s(52)), "Internet Money", font=serif, fill=PAPER)
    draw.text((s(243), s(134)), "P R O O F - O F - W O R K   M O N E Y   F O R   P A Y M E N T S   ·   I M N", font=mono, fill=SOFT)
    # Wave-line border along the foot, as on the pages
    for flip in (1, -1):
        draw.line(wave(width * scale, s(height - 14), s(6), s(34), flip), fill=MID, width=scale)
    image.resize((width, height), Image.LANCZOS).save(path)


def banner_svg():
    def path(flip):
        points = wave(1100, 226, 6, 34, flip)
        return "M" + " L".join(f"{px} {py:.1f}" for px, py in points)

    return f'''<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1100 240" width="1100" height="240">
  <rect width="1100" height="240" fill="{GREEN}"/>
  <g transform="translate(40 40) scale(0.3125)">
  {mark_svg(inverse=True, body_only=True)}
  </g>
  <text x="240" y="108" font-family="Fraunces, Georgia, serif" font-weight="600" font-size="62" fill="{PAPER}">Internet Money</text>
  <text x="243" y="150" font-family="'JetBrains Mono', Consolas, monospace" font-size="16" letter-spacing="4" fill="{SOFT}">PROOF-OF-WORK MONEY FOR PAYMENTS · IMN</text>
  <path d="{path(1)}" fill="none" stroke="{MID}" stroke-width="1"/>
  <path d="{path(-1)}" fill="none" stroke="{MID}" stroke-width="1"/>
</svg>
'''


def main():
    write = lambda name, text: open(os.path.join(HERE, name), "w", encoding="utf-8", newline="\n").write(text)
    write("imoney-icon.svg", mark_svg())
    write("imoney-icon-on-green.svg", mark_svg(inverse=True))
    write("imoney-logo-banner.svg", banner_svg())
    icon_png(os.path.join(HERE, "imoney-icon.png"))
    banner_png(os.path.join(HERE, "imoney-logo-banner.png"))
    # The website serves its own copies
    site = os.path.join(ROOT, "apps", "imoney-website")
    shutil.copyfile(os.path.join(HERE, "imoney-icon.svg"), os.path.join(site, "icon.svg"))
    shutil.copyfile(os.path.join(HERE, "imoney-logo-banner.png"), os.path.join(site, "banner.png"))


if __name__ == "__main__":
    main()
