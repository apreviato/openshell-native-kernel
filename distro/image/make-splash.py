#!/usr/bin/env python3
"""Draws splash.png, the screen GRUB shows while it loads the system.

OpenShell Live shows a Plymouth splash and then the desktop's boot screen
(openshell repo, apps/web/src/components/BootScreen.tsx), with the same
near-black, the OPENSHELL wordmark and the 200px bar in the same places.
Native has no Plymouth: GRUB draws this frame as its background, the kernel
keeps the framebuffer as it is (gfxpayload=keep, a quiet EFI stub), and the
desktop's boot screen follows. The pieces are Live's Plymouth images, placed
by its geometry (openshelllive scripts/make-boot-splash.py, measured at
1920x1080):

  wordmark  its left edge 151px left of the centre, baseline 18px under it
  bar       200x3, top 39px under the centre, the 60px segment in the middle
  status    centred, baseline 67px under the centre

The image is checked in, so building the media needs no Pillow:

  python3 distro/image/make-splash.py <openshelllive>/live/config/includes.chroot/usr/share/plymouth/themes/openshell
"""
import sys
from pathlib import Path

from PIL import Image

WIDTH, HEIGHT = 1920, 1080
BACKGROUND = (0x05, 0x06, 0x07)
WORDMARK_ASCENT = 48


def main(theme: Path) -> None:
    def piece(name: str) -> Image.Image:
        return Image.open(theme / name).convert("RGBA")

    centre_x, centre_y = WIDTH // 2, HEIGHT // 2
    frame = Image.new("RGBA", (WIDTH, HEIGHT), (*BACKGROUND, 255))
    mark = piece("wordmark.png")
    frame.alpha_composite(mark, (centre_x - 151, centre_y + 18 - WORDMARK_ASCENT))
    track = piece("bar-track.png")
    track_x, track_y = centre_x - track.width // 2, centre_y + 39
    frame.alpha_composite(track, (track_x, track_y))
    sweep = piece("bar-sweep.png")
    frame.alpha_composite(sweep, (track_x + (track.width - sweep.width) // 2, track_y))
    status = piece("status.png")
    frame.alpha_composite(status, (centre_x - status.width // 2, centre_y + 67 - 12))
    # GRUB reads 24-bit PNGs; no alpha.
    frame.convert("RGB").save(Path(__file__).with_name("splash.png"), optimize=True)


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    main(Path(sys.argv[1]))
