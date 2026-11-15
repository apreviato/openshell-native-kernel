#!/usr/bin/env python3
"""Draws menu.png, the background of the live medium's boot menu
(menu-theme.txt).

OpenShell Live's menu background is an SVG (openshelllive
live/config/bootloaders/splash.svg, 640x480): pure black, the OPENSHELL
wordmark (OPEN in the accent blue, SHELL in the text gray, DejaVu Sans Mono
Bold 66px, letter spacing 3), a 168x3 bar under it and "LIVE" in gray with
wide spacing. This one draws the same at 1920x1080, scaled by the height
(2.25), with "NATIVE"; the theme writes the release under it and the menu
below. The image is checked in, so building the media needs no Pillow:

  python3 distro/image/make-menu.py <DejaVuSansMono-Bold.ttf>
"""
import sys
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

WIDTH, HEIGHT = 1920, 1080
SCALE = HEIGHT / 480
ACCENT = (0x7A, 0xA2, 0xF7)
TEXT = (0xD4, 0xD4, 0xD4)
MUTED = (0x8F, 0x8F, 0x8F)


def spaced(draw, centre_x, baseline, parts, face, spacing):
    """Text centred on centre_x, each letter `spacing` apart beyond its own width."""
    letters = [(letter, colour) for text, colour in parts for letter in text]
    widths = [face.getlength(letter) for letter, _ in letters]
    total = sum(widths) + spacing * (len(letters) - 1)
    x = centre_x - total / 2
    for (letter, colour), width in zip(letters, widths):
        draw.text((x, baseline), letter, font=face, fill=colour, anchor="ls")
        x += width + spacing


def main(font_path):
    image = Image.new("RGB", (WIDTH, HEIGHT), (0, 0, 0))
    draw = ImageDraw.Draw(image)
    centre = WIDTH / 2
    wordmark = ImageFont.truetype(font_path, round(66 * SCALE))
    spaced(draw, centre, 150 * SCALE, [("OPEN", ACCENT), ("SHELL", TEXT)], wordmark, 3 * SCALE)
    bar_width, bar_height = 168 * SCALE, 3 * SCALE
    top = 172 * SCALE
    draw.rounded_rectangle((centre - bar_width / 2, top, centre + bar_width / 2, top + bar_height),
                           radius=bar_height / 2, fill=tuple(round(c * 0.85) for c in ACCENT))
    edition = ImageFont.truetype(font_path, round(17 * SCALE))
    spaced(draw, centre, 204 * SCALE, [("NATIVE", MUTED)], edition, 9 * SCALE)
    image.save(Path(__file__).with_name("menu.png"), optimize=True)


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    main(sys.argv[1])
