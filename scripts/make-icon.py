"""Draws the icon: the response of a two-path channel over time (the sweeping
notches the program makes), in the waterfall colours, on a rounded tile.

    python scripts/make-icon.py

Writes assets/icon_512.png, assets/icon.png (256) and assets/icon.ico.
"""
import numpy as np
from PIL import Image

N = 1024  # drawn large, then reduced
STOPS = [(0.0, (0, 0, 4)), (0.2, (40, 11, 84)), (0.4, (101, 21, 110)),
         (0.6, (159, 42, 99)), (0.8, (237, 105, 37)), (1.0, (252, 255, 164))]

t = np.linspace(0.0, 1.0, N)[None, :]          # time, left to right
f = np.linspace(1.0, 0.0, N)[:, None]          # frequency, low at the bottom
# Paths: (gain, delay in cycles across the tile, Doppler turns across the tile)
paths = [(1.0, 0.0, 0.0), (0.93, 2.1, 1.25)]
h = sum(g * np.exp(-2j * np.pi * (d * f + s * t)) for g, d, s in paths)
level = np.abs(h) / sum(g for g, _, _ in paths)
db = 20 * np.log10(np.maximum(level, 1e-3))
v = np.clip((db + 22) / 22, 0, 1) ** 1.6
# Darker towards the top, as a signal's edge would be.
v *= 0.62 + 0.38 * (1.0 - f) ** 0.7

xs = np.array([s[0] for s in STOPS])
rgb = np.stack([np.interp(v, xs, [s[1][c] for s in STOPS]) for c in range(3)], -1)

# Rounded tile.
r = 0.22 * N
y, x = np.mgrid[0:N, 0:N] + 0.5
dx = np.maximum(np.maximum(r - x, x - (N - r)), 0)
dy = np.maximum(np.maximum(r - y, y - (N - r)), 0)
alpha = np.clip(r - np.hypot(dx, dy) + 0.5, 0, 1)
img = Image.fromarray(np.dstack([rgb, alpha * 255]).astype(np.uint8), "RGBA")

big = img.resize((512, 512), Image.LANCZOS)
big.save("assets/icon_512.png")
big.resize((256, 256), Image.LANCZOS).save("assets/icon.png")
big.save("assets/icon.ico", sizes=[(s, s) for s in (16, 24, 32, 48, 64, 128, 256)])
