"""Makes Carrick's icons from packaging/icon-source.png (two arrows, blue and
white, tied in one knot on their way across): carrick.png (256 px, for Linux
and the README), carrick.ico (16 to 256 px, for Windows) and the window's own
icon, crates/carrick-gui/src/icon-64.rgba (64 x 64, raw RGBA). The drawing is
wider than it is tall, so it sits centred on a transparent square. Needs
Pillow.

    python packaging/icon.py
"""
from pathlib import Path

from PIL import Image

HERE = Path(__file__).parent
SOURCE = HERE / "icon-source.png"
WINDOW_ICON = HERE.parent / "crates" / "carrick-gui" / "src" / "icon-64.rgba"


def drawing() -> Image.Image:
    """The source, without its nearly invisible stray pixels, cut to the drawing."""
    img = Image.open(SOURCE).convert("RGBA")
    alpha = img.getchannel("A").point(lambda v: 0 if v <= 8 else v)
    img.putalpha(alpha)
    return img.crop(alpha.getbbox())


def square(art: Image.Image, size: int) -> Image.Image:
    """The drawing centred on a transparent square, with a small margin (none
    at the smallest sizes, where every pixel counts)."""
    margin = 0 if size <= 32 else round(size * 0.04)
    width = size - 2 * margin
    height = max(1, round(art.height * width / art.width))
    out = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    out.alpha_composite(art.resize((width, height), Image.LANCZOS), (margin, (size - height) // 2))
    return out


if __name__ == "__main__":
    art = drawing()
    square(art, 256).save(HERE / "carrick.png", optimize=True)
    sizes = [16, 24, 32, 48, 64, 128, 256]
    frames = [square(art, s) for s in sizes]
    frames[-1].save(HERE / "carrick.ico", sizes=[(s, s) for s in sizes], append_images=frames[:-1])
    WINDOW_ICON.write_bytes(square(art, 64).tobytes())
    print("carrick.png, carrick.ico and icon-64.rgba written")
