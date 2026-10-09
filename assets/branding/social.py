"""Draws the social-media images from the same mark and colours as render.py.

    python assets/branding/social.py

Everything lands in assets/branding/social/. Sizes follow what each platform asks for; the
content sits in the middle of each image, clear of the edges platforms crop or cover.
"""
import os

from PIL import Image, ImageDraw

from render import GREEN, HERE, MID, PAPER, SOFT, draw_mark, font, wave

OUT = os.path.join(HERE, "social")
SERIF = ["georgiab.ttf", "DejaVuSerif-Bold.ttf", "LiberationSerif-Bold.ttf"]
MONO = ["consola.ttf", "DejaVuSansMono.ttf", "LiberationMono-Regular.ttf"]
TAGLINE = "PROOF-OF-WORK MONEY FOR PAYMENTS"


def spaced(text):
    """Letter-spaced capitals, as on the banner."""
    return " ".join(text).replace("   ", "     ")


def avatar(name, size=800, inverse=False):
    """A square that still reads when a platform crops it to a circle: the framed mark is
    drawn smaller than the square, so its corners stay inside the circle."""
    scale = 3
    field = PAPER if inverse else GREEN
    image = Image.new("RGB", (size * scale, size * scale), field)
    mark = int(size * scale * 0.64)
    offset = (size * scale - mark) // 2
    draw_mark(ImageDraw.Draw(image), offset, offset, mark, inverse=inverse)
    image.resize((size, size), Image.LANCZOS).save(os.path.join(OUT, name))


def card(name, width, height, content_height, line=None):
    """A banner with the mark, the name and a line beneath, centred. `content_height` is how
    tall the block may be: the part of the image every device shows."""
    scale = 2
    image = Image.new("RGB", (width * scale, height * scale), GREEN)
    draw = ImageDraw.Draw(image)
    s = lambda v: int(v * scale)

    mark = content_height * 0.62
    title_font = font(SERIF, s(content_height * 0.30))
    line_font = font(MONO, s(content_height * 0.075))
    line_text = spaced(line or (TAGLINE + " · IMN"))
    gap = content_height * 0.16

    title_width = draw.textlength("Internet Money", font=title_font) / scale
    line_width = draw.textlength(line_text, font=line_font) / scale
    block = mark + gap + max(title_width, line_width)
    left = (width - block) / 2
    top = (height - mark) / 2

    draw_mark(draw, s(left), s(top), s(mark), inverse=True)
    text_left = left + mark + gap
    draw.text((s(text_left), s(top + mark * 0.08)), "Internet Money", font=title_font, fill=PAPER)
    draw.text((s(text_left + 3), s(top + mark * 0.72)), line_text, font=line_font, fill=SOFT)

    # The wave-line border of the pages, top and bottom
    period, amplitude = s(max(24, height * 0.07)), s(max(4, height * 0.012))
    for edge in (s(height * 0.045), s(height * (1 - 0.045))):
        for flip in (1, -1):
            draw.line(wave(width * scale, edge, amplitude, period, flip), fill=MID, width=scale)
    image.resize((width, height), Image.LANCZOS).save(os.path.join(OUT, name))


def main():
    os.makedirs(OUT, exist_ok=True)
    avatar("avatar.png")
    avatar("avatar-light.png", inverse=True)
    card("x-header-1500x500.png", 1500, 500, 230)
    card("github-social-preview-1280x640.png", 1280, 640, 210)
    card("link-preview-1200x630.png", 1200, 630, 200)
    card("discord-banner-960x540.png", 960, 540, 160)
    card("linkedin-cover-1128x191.png", 1128, 191, 110)
    card("reddit-banner-1920x384.png", 1920, 384, 190)
    # YouTube shows only the middle 1546 x 423 on every device
    card("youtube-banner-2560x1440.png", 2560, 1440, 300)
    card("telegram-post-1280x720.png", 1280, 720, 220, line="TEST NETWORK RUNNING · NO PREMINE · OPEN SOURCE")
    print("wrote", OUT)


if __name__ == "__main__":
    main()
