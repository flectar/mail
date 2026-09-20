#!/usr/bin/env python3
"""Generate and check the app's Lucide outline and solid families."""

import argparse
from pathlib import Path
import re
import sys

from solid_engine import Renderer, Sources
from solid_recipes import build


ROOT = Path(__file__).resolve().parent


def outputs(root=ROOT):
    sources = Sources(root)
    recipes = build(sources)
    return sources, recipes, {name: Renderer().svg(recipe.drawing, custom=recipe.source is None)
                              for name, recipe in sorted(recipes.items())}


def outline_outputs(sources):
    return {name: sources.outline(item['source'])
            for name, item in sorted(sources.manifest['outlines'].items())}


def check_usage(families, ui):
    """Verify that every referenced Lucide asset belongs to a generated family."""
    used = set()
    for path in ui.rglob("*.slint"):
        used.update(re.findall(r'icons/lucide/([^/"\s]+/[^/"\s]+\.svg)', path.read_text()))
    available = {f'{family}/{name}' for family, assets in families.items() for name in assets}
    missing = used - available
    if missing:
        raise ValueError("Missing generated UI assets: " + ", ".join(sorted(missing)))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--check', action='store_true', help='Read-only freshness, source and coverage checks')
    parser.add_argument('--verify-upstream', type=Path, metavar='ICONS_DIR',
                        help='Compare inputs with an unpacked lucide-static 1.46.0 icons directory')
    parser.add_argument('--parts', metavar='SOURCE', help='Print numbered original elements for writing a recipe')
    args = parser.parse_args()
    try:
        sources, _, generated = outputs()
        if args.parts:
            for index, element in enumerate(sources.trees[args.parts]):
                print(index, element.tag.rsplit('}', 1)[-1], dict(element.attrib))
            return 0
        if args.verify_upstream:
            for name, item in sources.manifest['sources'].items():
                if (ROOT / item['path']).read_bytes() != (args.verify_upstream / f'{name}.svg').read_bytes():
                    raise ValueError(f'Upstream mismatch: {name}')
        families = {'outline': outline_outputs(sources),
                    'filled': {f'{name}.svg': content for name, content in generated.items()}}
        check_usage(families, ROOT.parents[1])
        for family, assets in families.items():
            unexpected = {p.name for p in (ROOT / family).glob('*.svg')} - set(assets)
            if unexpected:
                raise ValueError(f'Unmanaged {family} assets (not deleted): ' + ', '.join(sorted(unexpected)))
        stale = []
        for family, assets in families.items():
            for filename, content in assets.items():
                path = ROOT / family / filename
                if not path.exists() or path.read_text() != content:
                    stale.append(f'{family}/{filename}')
                    if not args.check:
                        path.parent.mkdir(exist_ok=True)
                        path.write_text(content)
        if args.check and stale:
            raise ValueError('Stale generated icons: ' + ', '.join(stale))
        print(f'{"Checked" if args.check else "Generated"} {len(families["outline"])} outlines and {len(generated)} solids; '
              f'{len(stale)} {"stale" if args.check else "updated"}; upstream inputs verified')
        return 0
    except (ValueError, KeyError, IndexError, OSError) as error:
        print(f'error: {error}', file=sys.stderr)
        return 1


if __name__ == '__main__':
    sys.exit(main())
