# The brand of Rature

The mark is an R written by hand with a wide parallel pen held at 35°: the stem, then a flag
that sweeps in from the left over its top, the bowl, the leg and a flat exit. Cream on a near
black ground. The SVGs of this folder are the one source of the drawing; the PNGs are rendered
from them by `tools/brand.sh` and committed. To change the brand, edit the SVGs and run it again.

| Source | For |
| --- | --- |
| `mark.svg` | the mark alone, for a dark background (the default) |
| `mark-on-light.svg` | the mark alone, for a light background |
| `icon.svg` | the icon: the mark on its dark tile, in a rounded square; the launcher's icon and the start page's |
| `favicon.svg` | the icon drawn for 16 to 64 pixels: a simpler R, thicker and without its finest turns |
| `social-preview.svg` | the social preview of the repository (1280 by 640): the icon, the name and the tagline |
| `social-preview-fr.svg` | the same with the French tagline, for French pages |

The colours: ink `#EDE6D8` on the tile `#161412`, `#1B1A18` on a light ground. The name is set
in Geist SemiBold and the tagline in Geist Regular (SIL Open Font License), both turned into
outlines: no SVG of this folder needs a font.

## What is generated

`png/`: the icon at 128, 256, 512 and 1024 pixels (`icon-*.png`), the favicon at 16, 32 and 64,
the marks at 512, `social-preview.png` and `social-preview-fr.png` (1280 by 640).
`tools/brand.sh` needs `rsvg-convert` (librsvg), and also copies `icon.svg` to
`packaging/linux/icons/hicolor/scalable/apps/rature.svg`, the icon `install.sh` installs.

`png/social-preview.png` is uploaded by hand in the repository's settings on GitHub (Settings,
General, Social preview), as GitHub offers no other way.
