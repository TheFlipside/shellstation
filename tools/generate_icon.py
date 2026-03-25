"""Generate ShellStation app icon in all required sizes."""

from PIL import Image, ImageDraw, ImageFont
import struct
import io
import os

ICON_DIR = "src-tauri/icons"


def draw_icon(size: int) -> Image.Image:
    """Draw the ShellStation icon at a given size.

    Design: dark rounded square with terminal prompt '>_' and
    a subtle satellite/signal arc in the top-right corner.
    Uses Catppuccin Mocha palette.
    """
    scale = size / 512  # design at 512, scale everything
    img = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    draw = ImageDraw.Draw(img)

    # Background: dark rounded rectangle
    bg_color = (30, 30, 46)  # Catppuccin Mocha base #1e1e2e
    border_color = (49, 50, 68)  # Surface0 #313244
    corner_radius = int(90 * scale)

    # Outer border/glow
    draw.rounded_rectangle(
        [int(4 * scale), int(4 * scale), size - int(4 * scale), size - int(4 * scale)],
        radius=corner_radius,
        fill=border_color,
    )

    # Inner background
    margin = int(8 * scale)
    draw.rounded_rectangle(
        [margin, margin, size - margin, size - margin],
        radius=int(85 * scale),
        fill=bg_color,
    )

    # Subtle gradient effect: slightly lighter strip at top
    for i in range(int(60 * scale)):
        alpha = int(12 * (1 - i / (60 * scale)))
        y = margin + i
        draw.line(
            [(margin + int(40 * scale), int(y)), (size - margin - int(40 * scale), int(y))],
            fill=(205, 214, 244, alpha),
        )

    # Terminal prompt: >_
    prompt_color = (137, 180, 250)  # Catppuccin Blue #89b4fa
    cursor_color = (166, 227, 161)  # Catppuccin Green #a6e3a1

    # Draw '>' as lines (chevron)
    cx = size * 0.28  # start x
    cy = size * 0.50  # center y
    chevron_w = size * 0.16
    chevron_h = size * 0.18
    line_w = max(int(28 * scale), 2)

    # Top stroke of >
    draw.line(
        [(int(cx), int(cy - chevron_h)), (int(cx + chevron_w), int(cy))],
        fill=prompt_color,
        width=line_w,
    )
    # Bottom stroke of >
    draw.line(
        [(int(cx), int(cy + chevron_h)), (int(cx + chevron_w), int(cy))],
        fill=prompt_color,
        width=line_w,
    )

    # Draw '_' (underscore cursor)
    underscore_x = size * 0.50
    underscore_y = cy + chevron_h
    underscore_w = size * 0.18
    draw.line(
        [
            (int(underscore_x), int(underscore_y)),
            (int(underscore_x + underscore_w), int(underscore_y)),
        ],
        fill=cursor_color,
        width=line_w,
    )

    # Signal arcs in top-right corner (the "station" motif)
    arc_color_1 = (180, 190, 254)  # Lavender #b4befe
    arc_color_2 = (137, 180, 250)  # Blue #89b4fa
    arc_color_3 = (116, 199, 236)  # Sapphire #74c7ec

    arc_cx = size * 0.78
    arc_cy = size * 0.22
    arc_w = max(int(12 * scale), 1)

    for i, (radius, color) in enumerate([
        (int(30 * scale), arc_color_3),
        (int(55 * scale), arc_color_2),
        (int(80 * scale), arc_color_1),
    ]):
        bbox = [
            int(arc_cx - radius),
            int(arc_cy - radius),
            int(arc_cx + radius),
            int(arc_cy + radius),
        ]
        draw.arc(bbox, start=290, end=20, fill=color, width=arc_w)

    # Small dot at signal origin
    dot_r = max(int(8 * scale), 1)
    draw.ellipse(
        [
            int(arc_cx - dot_r),
            int(arc_cy - dot_r),
            int(arc_cx + dot_r),
            int(arc_cy + dot_r),
        ],
        fill=arc_color_3,
    )

    return img


def create_ico(images: list[tuple[int, Image.Image]], path: str) -> None:
    """Create ICO file from list of (size, image) tuples."""
    # Use Pillow's built-in ICO save
    # Sort largest first for quality
    imgs = [img for _, img in sorted(images, key=lambda x: x[0], reverse=True)]
    imgs[0].save(path, format="ICO", sizes=[(img.width, img.height) for img in imgs])


def main() -> None:
    os.makedirs(ICON_DIR, exist_ok=True)

    # Tauri required sizes
    standard_sizes = {
        "32x32.png": 32,
        "128x128.png": 128,
        "128x128@2x.png": 256,
        "icon.png": 512,
    }

    # Windows Store sizes
    store_sizes = {
        "Square30x30Logo.png": 30,
        "Square44x44Logo.png": 44,
        "Square71x71Logo.png": 71,
        "Square89x89Logo.png": 89,
        "Square107x107Logo.png": 107,
        "Square142x142Logo.png": 142,
        "Square150x150Logo.png": 150,
        "Square284x284Logo.png": 284,
        "Square310x310Logo.png": 310,
        "StoreLogo.png": 50,
    }

    all_sizes = {**standard_sizes, **store_sizes}
    ico_images = []

    for filename, size in all_sizes.items():
        img = draw_icon(size)
        filepath = os.path.join(ICON_DIR, filename)
        img.save(filepath, "PNG")
        print(f"  {filepath} ({size}x{size})")

        if size <= 256:
            ico_images.append((size, img))

    # ICO file (Windows)
    ico_path = os.path.join(ICON_DIR, "icon.ico")
    create_ico(ico_images, ico_path)
    print(f"  {ico_path} (ICO)")

    # ICNS is macOS-specific; generate a basic one from 512px
    # Pillow doesn't support ICNS natively, so we create it manually
    # For now, just copy the 512px as the icns source
    # Tauri will handle icns generation from icon.png during macOS builds
    icon_512 = draw_icon(512)
    icon_512.save(os.path.join(ICON_DIR, "icon.png"), "PNG")

    # Also create a 1024px master for ICNS
    icon_1024 = draw_icon(1024)
    try:
        icon_1024.save(os.path.join(ICON_DIR, "icon.icns"), format="ICNS")
        print(f"  {ICON_DIR}/icon.icns (ICNS)")
    except Exception:
        # Pillow may not support ICNS writing; that's OK
        # macOS builds will use icon.png
        print("  (ICNS generation skipped - will use icon.png for macOS builds)")

    # Create favicon SVG for the webview
    # (The actual app icon is handled by Tauri, this is just for dev)
    print("\nDone! All icons generated.")


if __name__ == "__main__":
    main()
