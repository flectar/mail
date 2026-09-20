# Lucide icons

Flectar Mail uses Lucide outlines and custom filled variants. SVGs are generated
using the unmodified `lucide-static` 1.46.0 assets in `sources/` and custom recipes.
Source hashes and outline mappings are recorded in `sources.json`; filled
mappings are defined in `solid_recipes.py`. Licensing is included in [LICENSE](LICENSE).

## Size and stroke

Each icon has one filename per family, such as `outline/calendar.svg` and
`filled/calendar.svg`. Slint controls the display size. Standard action icons,
sidebar navigation icons and the desktop product rail use the shared 18px
`LayoutMetrics.icon-size`; compact navigation uses 22px.

Outlines retain Lucide's original paths and 24×24 viewBox, with
`stroke-width="1.8"`. At 16px this produces a 1.2px visible stroke:
`1.8 × 16 / 24 = 1.2`. At the standard 18px display size the visible stroke is
1.35px. The stroke scales proportionally at other display sizes.

`DISPLAY_SIZE` and `DISPLAY_STROKE_PX` in `solid_engine.py` define the outline
weight. Filled recipes use separate settings and the original source geometry.

## Filled variants

`solid_recipes.py` defines filled variants by selecting source elements and
combining four operations from `solid_engine.py`:

| Operation | Result |
| --- | --- |
| `solid(shape)` | Filled silhouette |
| `line(shape, width)` | Open stroke, including active formatting controls |
| `cut(body, details)` | Transparent interior detail |
| `over(back, front, gap)` | Solid foreground separated from the rear object |

The default separation is 1.5 viewBox units, equivalent to 1px at 16px. Cutouts
use SVG luminance masks so the background remains visible. Slint applies icon
colors through the resulting alpha channel. Line-only active controls use a
3-unit stroke.

Recipes explicitly define contour closures, overlapping objects and optical
adjustments. Changes to upstream geometry require updating the pinned hashes and
reviewing the affected recipes. Generated SVGs are checked into the repository;
normal application builds do not require Python or network access for icons.

Custom alternatives use the `-flectar` suffix, such as `filled/inbox-flectar.svg`,
and are generated from `solid_recipes.py` alongside the Lucide variants.

## Generation

From the repository root, using the Python standard library:

```sh
python3 ui/icons/lucide/generate.py
python3 ui/icons/lucide/generate.py --check
```

Generation writes changed assets only. `--check` verifies source hashes,
generated outputs and UI references without writing files. Unmanaged SVGs are
reported rather than deleted. `--parts SOURCE` lists upstream elements for recipe
development; `--verify-upstream /path/to/package/icons` compares the vendored
sources with an unpacked `lucide-static` 1.46.0 package.

## Validation

```sh
python3 -m unittest discover -s ui/icons/lucide -p 'test_*.py'
```

Raster checks run when `resvg` and ImageMagick `convert` are available; `RESVG`
can specify the renderer's path. They cover transparent separation, exterior
foreground shapes, interior cutouts and rendering at 16px and 24px. Otherwise,
raster checks are skipped while source, generation and reference checks run.
