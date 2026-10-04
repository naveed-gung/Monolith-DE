"""Bake rounded corners into Monolith's static square artwork.

Slint's software renderer clips to rectangular bounds only: `clip: true`
combined with `border-radius` silently renders square corners. Static images
therefore carry their own rounded alpha channel instead of relying on the
renderer to clip them.

Run from the repository root:  python tools/round_assets.py
"""

from pathlib import Path

from PIL import Image, ImageDraw

ROOT = Path(__file__).resolve().parent.parent
ASSETS = ROOT / "assets"
SUPERSAMPLE = 4

try:
    RESAMPLE = Image.Resampling.LANCZOS
except AttributeError:  # Pillow older than 9.1
    RESAMPLE = Image.LANCZOS  # type: ignore[attr-defined]


def rounded(source: Path, target: Path, size: int, radius: int) -> None:
    image = Image.open(source).convert("RGBA").resize((size, size), RESAMPLE)

    # Draw the mask oversized, then downscale it, so the arc edges are
    # anti-aliased instead of visibly stepped.
    mask = Image.new("L", (size * SUPERSAMPLE, size * SUPERSAMPLE), 0)
    ImageDraw.Draw(mask).rounded_rectangle(
        [0, 0, size * SUPERSAMPLE - 1, size * SUPERSAMPLE - 1],
        radius=radius * SUPERSAMPLE,
        fill=255,
    )
    mask = mask.resize((size, size), RESAMPLE)

    output = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    output.paste(image, (0, 0), mask)
    output.save(target)

    alpha = output.getchannel("A")
    print(
        f"{target.relative_to(ROOT)} {output.size} "
        f"corner={alpha.getpixel((0, 0))} "
        f"edge={alpha.getpixel((size // 2, 0))} "
        f"center={alpha.getpixel((size // 2, size // 2))}"
    )


def main() -> None:
    # 10 px corner on the 36 px rail logo, kept at 4x for crisp downscaling.
    rounded(ASSETS / "monolith.png", ASSETS / "monolith-mark.png", 256, 71)
    # A second copy at twice the rail's display size. The software renderer
    # scales a 256 px source down to 36 px poorly, and the mark reads as
    # pixelated; an exact 2:1 reduction stays clean.
    rounded(ASSETS / "monolith.png", ASSETS / "monolith-mark-72.png", 72, 20)
    # 14 px corner on the 60 px developer portrait.
    if (ASSETS / "profile.jpg").is_file():
        rounded(ASSETS / "profile.jpg", ASSETS / "profile-card.png", 240, 56)


if __name__ == "__main__":
    main()
