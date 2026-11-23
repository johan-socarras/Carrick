"""Draws Carrick's icon (the one the window draws in main.rs: two white dots
joined by a line, on a blue rounded square) as carrick.png (256 px) and
carrick.ico (16 to 256 px), for the installers. Needs Pillow.

    python packaging/icon.py
"""
from pathlib import Path

from PIL import Image

HERE = Path(__file__).parent
N = 64  # the design is on a 64-unit grid, like main.rs
BLUE = (0x2B, 0x6C, 0xD6, 255)
WHITE = (255, 255, 255, 255)


def draw(size: int) -> Image.Image:
    img = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    px = img.load()
    s = N / size
    for py in range(size):
        for qx in range(size):
            # Sample the 64-unit design at the pixel centre.
            x, y = (qx + 0.5) * s, (py + 0.5) * s
            r = 14.0
            cx = min(max(x, 2.0 + r), 62.0 - r)
            cy = min(max(y, 2.0 + r), 62.0 - r)
            if (x - cx) ** 2 + (y - cy) ** 2 > r * r:
                continue

            def near(ax, ay, radius):
                return (x - ax) ** 2 + (y - ay) ** 2 <= radius * radius

            line = 20.0 <= x <= 44.0 and abs(y - 32.0) <= 2.5
            white = near(18.0, 32.0, 7.0) or near(46.0, 32.0, 7.0) or line
            px[qx, py] = WHITE if white else BLUE
    return img


if __name__ == "__main__":
    big = draw(1024).resize((256, 256), Image.LANCZOS)
    big.save(HERE / "carrick.png")
    big.save(HERE / "carrick.ico", sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)])
    print("carrick.png and carrick.ico written")
