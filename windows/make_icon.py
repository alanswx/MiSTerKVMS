"""Windows app icon from the macOS one (apple/Icon/AppIcon.png, made by
apple/Icon/make_icon.py): writes Assets/AppIcon.ico (16-256 px) and a PNG
copy for the About dialog.

    python windows/make_icon.py      # needs Pillow
"""
from pathlib import Path

from PIL import Image

ROOT = Path(__file__).resolve().parent.parent
SRC = ROOT / "apple" / "Icon" / "AppIcon.png"
OUT = ROOT / "windows" / "MiSTerKVMS" / "Assets"

OUT.mkdir(parents=True, exist_ok=True)
image = Image.open(SRC).convert("RGBA")
image.save(OUT / "AppIcon.ico", sizes=[(s, s) for s in (16, 24, 32, 48, 64, 128, 256)])
image.resize((256, 256), Image.Resampling.LANCZOS).save(OUT / "AppIcon.png")
print(f"wrote {OUT / 'AppIcon.ico'} and AppIcon.png")
