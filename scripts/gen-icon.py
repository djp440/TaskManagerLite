"""生成 assets/app.ico 与 assets/logo.png，图案与托盘图标（tray.rs render_icon）一致。用法：python -I scripts/gen-icon.py"""
from PIL import Image, ImageDraw
import os
BG = (0x4C, 0x9B, 0xF5, 255)
LINE = [(0.14, 0.66), (0.32, 0.66), (0.43, 0.30), (0.56, 0.78), (0.67, 0.46), (0.86, 0.46)]
SS = 4

def render(size):
    n = size * SS
    img = Image.new("RGBA", (n, n), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    d.rounded_rectangle([0, 0, n - 1, n - 1], radius=n * 0.24, fill=BG)
    pts = [(x * n, y * n) for x, y in LINE]
    w = max(int(n * 0.15), 2)
    d.line(pts, fill=(255, 255, 255, 255), width=w, joint="curve")
    for x, y in pts:
        r = w / 2
        d.ellipse([x - r, y - r, x + r, y + r], fill=(255, 255, 255, 255))
    return img.resize((size, size), Image.LANCZOS)

out = os.path.join(os.path.dirname(__file__), "..", "assets")
os.makedirs(out, exist_ok=True)
render(512).save(os.path.join(out, "logo.png"))
render(256).save(os.path.join(out, "app.ico"), sizes=[(s, s) for s in (16, 24, 32, 48, 64, 128, 256)])
