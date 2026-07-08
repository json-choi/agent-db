from __future__ import annotations

import shutil
import subprocess
from pathlib import Path

from PIL import Image, ImageDraw, ImageFilter


ROOT = Path(__file__).resolve().parents[1]
SITE_PUBLIC = ROOT / "site" / "public"
TAURI_ICONS = ROOT / "src-tauri" / "icons"
SOURCE_SIZE = 2048


def rgb(hex_color: str) -> tuple[int, int, int]:
    value = hex_color.removeprefix("#")
    return tuple(int(value[i : i + 2], 16) for i in (0, 2, 4))


def vertical_gradient(size: tuple[int, int], top: str, bottom: str) -> Image.Image:
    width, height = size
    a = rgb(top)
    b = rgb(bottom)
    image = Image.new("RGBA", size)
    pixels = image.load()
    for y in range(height):
        t = y / max(height - 1, 1)
        color = tuple(round(a[i] * (1 - t) + b[i] * t) for i in range(3))
        for x in range(width):
            pixels[x, y] = (*color, 255)
    return image


def radial_glow(size: int, color: str, alpha: int, radius: float, center: tuple[float, float]) -> Image.Image:
    layer = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    draw = ImageDraw.Draw(layer)
    cx, cy = center
    r = int(size * radius)
    draw.ellipse(
        (int(cx * size) - r, int(cy * size) - r, int(cx * size) + r, int(cy * size) + r),
        fill=(*rgb(color), alpha),
    )
    return layer.filter(ImageFilter.GaussianBlur(int(size * 0.11)))


def rounded_mask(size: int, radius: int) -> Image.Image:
    mask = Image.new("L", (size, size), 0)
    ImageDraw.Draw(mask).rounded_rectangle((0, 0, size, size), radius=radius, fill=255)
    return mask


def ellipse_mask(size: tuple[int, int]) -> Image.Image:
    mask = Image.new("L", size, 0)
    ImageDraw.Draw(mask).ellipse((0, 0, size[0] - 1, size[1] - 1), fill=255)
    return mask


def paste_masked(base: Image.Image, layer: Image.Image, mask: Image.Image, xy: tuple[int, int]) -> None:
    base.alpha_composite(Image.composite(layer, Image.new("RGBA", layer.size), mask), xy)


def render_icon(size: int = SOURCE_SIZE) -> Image.Image:
    s = size
    icon = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    tile_mask = rounded_mask(s, int(s * 0.225))

    tile = vertical_gradient((s, s), "#1a231c", "#0b0f0c")
    tile.alpha_composite(radial_glow(s, "#6fd26c", 82, 0.55, (0.33, 0.28)))
    tile.alpha_composite(radial_glow(s, "#295f3a", 70, 0.65, (0.78, 0.82)))

    grid = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    grid_draw = ImageDraw.Draw(grid)
    step = int(s * 0.078)
    for pos in range(-step, s + step, step):
        grid_draw.line((pos, 0, pos + int(s * 0.28), s), fill=(255, 255, 255, 10), width=max(1, s // 360))
        grid_draw.line((0, pos, s, pos), fill=(255, 255, 255, 6), width=max(1, s // 420))
    tile.alpha_composite(grid)

    bevel = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    bevel_draw = ImageDraw.Draw(bevel)
    inset = int(s * 0.03)
    bevel_draw.rounded_rectangle(
        (inset, inset, s - inset, s - inset),
        radius=int(s * 0.2),
        outline=(255, 255, 255, 32),
        width=max(2, s // 110),
    )
    bevel_draw.arc(
        (int(s * 0.11), int(s * 0.08), int(s * 0.89), int(s * 0.66)),
        190,
        345,
        fill=(255, 255, 255, 26),
        width=max(2, s // 120),
    )
    tile.alpha_composite(bevel)
    paste_masked(icon, tile, tile_mask, (0, 0))

    shadow = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    shadow_draw = ImageDraw.Draw(shadow)
    shadow_draw.ellipse(
        (int(s * 0.22), int(s * 0.72), int(s * 0.78), int(s * 0.91)),
        fill=(0, 0, 0, 126),
    )
    icon.alpha_composite(shadow.filter(ImageFilter.GaussianBlur(int(s * 0.045))))

    x = int(s * 0.245)
    y = int(s * 0.205)
    w = int(s * 0.51)
    h = int(s * 0.56)
    top_h = int(s * 0.19)
    stroke = max(12, int(s * 0.047))

    body_mask = Image.new("L", (s, s), 0)
    body_draw = ImageDraw.Draw(body_mask)
    body_draw.rectangle((x, y + top_h // 2, x + w, y + h - top_h // 2), fill=255)
    body_draw.ellipse((x, y + h - top_h, x + w, y + h), fill=255)
    body = vertical_gradient((s, s), "#62de72", "#13883c")
    body_shadow = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    body_shadow_draw = ImageDraw.Draw(body_shadow)
    body_shadow_draw.rectangle((x, y, x + int(w * 0.18), y + h), fill=(0, 0, 0, 46))
    body_shadow_draw.rectangle((x + int(w * 0.78), y, x + w, y + h), fill=(0, 0, 0, 56))
    body_shadow_draw.rectangle((x + int(w * 0.35), y, x + int(w * 0.65), y + h), fill=(255, 255, 255, 20))
    body.alpha_composite(body_shadow)
    paste_masked(icon, body, body_mask, (0, 0))

    draw = ImageDraw.Draw(icon)
    outline = "#79f17b"
    dark_edge = "#0c6d31"
    cream = "#fbfbf7"

    draw.line((x, y + top_h // 2, x, y + h - top_h // 2), fill=outline, width=stroke)
    draw.line((x + w, y + top_h // 2, x + w, y + h - top_h // 2), fill=dark_edge, width=stroke)
    draw.arc((x, y + h - top_h, x + w, y + h), 0, 180, fill=outline, width=stroke)

    top = Image.new("RGBA", (w + 1, top_h + 1), (0, 0, 0, 0))
    top_grad = vertical_gradient((w + 1, top_h + 1), "#91f285", "#3fca60")
    top_mask = ellipse_mask((w + 1, top_h + 1))
    paste_masked(top, top_grad, top_mask, (0, 0))
    top_draw = ImageDraw.Draw(top)
    top_draw.ellipse(
        (int(w * 0.08), int(top_h * 0.16), int(w * 0.92), int(top_h * 0.56)),
        fill=(255, 255, 255, 42),
    )
    icon.alpha_composite(top, (x, y))
    draw.ellipse((x, y, x + w, y + top_h), outline="#99fb8e", width=max(8, int(s * 0.018)))

    mid = (x, y + int(top_h * 1.16), x + w, y + int(top_h * 2.22))
    draw.arc(mid, 0, 180, fill=cream, width=max(14, int(s * 0.038)))
    draw.arc(
        (x + int(w * 0.04), y + int(top_h * 2.48), x + int(w * 0.96), y + int(top_h * 3.32)),
        0,
        180,
        fill=(255, 255, 255, 70),
        width=max(4, int(s * 0.011)),
    )
    draw.arc(
        (x + int(w * 0.08), y + int(top_h * 3.1), x + int(w * 0.92), y + int(top_h * 3.9)),
        0,
        180,
        fill=(145, 242, 133, 150),
        width=max(5, int(s * 0.015)),
    )

    highlight = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    high_draw = ImageDraw.Draw(highlight)
    high_draw.rounded_rectangle(
        (x + int(w * 0.63), y + int(top_h * 0.72), x + int(w * 0.71), y + int(h * 0.72)),
        radius=int(s * 0.035),
        fill=(255, 255, 255, 38),
    )
    icon.alpha_composite(highlight.filter(ImageFilter.GaussianBlur(max(2, s // 360))))

    for idx in range(5):
        cx = x + int(w * (0.25 + idx * 0.125))
        cy = y + int(h * 0.82)
        r = max(5, int(s * 0.012))
        draw.ellipse((cx - r, cy - r, cx + r, cy + r), fill=(245, 255, 242, 118))

    icon = icon.resize((1024, 1024), Image.Resampling.LANCZOS)
    return icon


def save_png(source: Image.Image, path: Path, size: int) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    source.resize((size, size), Image.Resampling.LANCZOS).save(path)


def write_favicon_svg(path: Path) -> None:
    path.write_text(
        """<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64" role="img" aria-label="DopeDB">
  <defs>
    <linearGradient id="bg" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0" stop-color="#1a231c"/>
      <stop offset="1" stop-color="#0b0f0c"/>
    </linearGradient>
    <linearGradient id="body" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0" stop-color="#62de72"/>
      <stop offset="1" stop-color="#13883c"/>
    </linearGradient>
    <linearGradient id="top" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0" stop-color="#91f285"/>
      <stop offset="1" stop-color="#3fca60"/>
    </linearGradient>
    <filter id="shadow" x="-30%" y="-30%" width="160%" height="170%">
      <feDropShadow dx="0" dy="4" stdDeviation="3" flood-color="#000" flood-opacity=".45"/>
    </filter>
  </defs>
  <rect width="64" height="64" rx="14" fill="url(#bg)"/>
  <circle cx="26" cy="20" r="23" fill="#6fd26c" opacity=".18"/>
  <g filter="url(#shadow)">
    <path d="M15 21h34v25c0 5-7.6 9-17 9s-17-4-17-9z" fill="url(#body)"/>
    <ellipse cx="32" cy="21" rx="17" ry="8" fill="url(#top)"/>
    <path d="M15 21v25c0 5 7.6 9 17 9s17-4 17-9V21" fill="none" stroke="#79f17b" stroke-width="5" stroke-linejoin="round"/>
    <path d="M15 32c0 5 7.6 9 17 9s17-4 17-9" fill="none" stroke="#fbfbf7" stroke-width="4" stroke-linecap="round"/>
    <path d="M19 43c2.5 3 7.3 4.8 13 4.8S42.5 46 45 43" fill="none" stroke="#bdf7bd" stroke-width="1.6" stroke-linecap="round" opacity=".65"/>
    <path d="M39 28v18" stroke="#fff" stroke-width="3" stroke-linecap="round" opacity=".2"/>
  </g>
</svg>
""",
        encoding="utf-8",
    )


def generate_icns(source: Image.Image) -> None:
    iconset = TAURI_ICONS / "icon.iconset"
    if iconset.exists():
        shutil.rmtree(iconset)
    iconset.mkdir(parents=True)
    specs = {
        "icon_16x16.png": 16,
        "icon_16x16@2x.png": 32,
        "icon_32x32.png": 32,
        "icon_32x32@2x.png": 64,
        "icon_128x128.png": 128,
        "icon_128x128@2x.png": 256,
        "icon_256x256.png": 256,
        "icon_256x256@2x.png": 512,
        "icon_512x512.png": 512,
        "icon_512x512@2x.png": 1024,
    }
    for name, size in specs.items():
        save_png(source, iconset / name, size)
    subprocess.run(["iconutil", "-c", "icns", str(iconset), "-o", str(TAURI_ICONS / "icon.icns")], check=True)
    shutil.rmtree(iconset)


def main() -> None:
    SITE_PUBLIC.mkdir(parents=True, exist_ok=True)
    TAURI_ICONS.mkdir(parents=True, exist_ok=True)
    source = render_icon()

    save_png(source, TAURI_ICONS / "icon.png", 1024)
    save_png(source, TAURI_ICONS / "32x32.png", 32)
    save_png(source, TAURI_ICONS / "128x128.png", 128)
    save_png(source, TAURI_ICONS / "128x128@2x.png", 256)
    source.save(TAURI_ICONS / "icon.ico", sizes=[(16, 16), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)])
    generate_icns(source)

    save_png(source, SITE_PUBLIC / "favicon-48x48.png", 48)
    save_png(source, SITE_PUBLIC / "apple-touch-icon.png", 180)
    save_png(source, SITE_PUBLIC / "icon-192.png", 192)
    save_png(source, SITE_PUBLIC / "icon-512.png", 512)
    source.save(SITE_PUBLIC / "favicon.ico", sizes=[(16, 16), (32, 32), (48, 48), (64, 64)])
    write_favicon_svg(SITE_PUBLIC / "favicon.svg")

    print("generated DopeDB app and web icons")


if __name__ == "__main__":
    main()
