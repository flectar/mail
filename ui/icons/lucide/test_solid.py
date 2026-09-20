"""Run with unittest; set RESVG to enable the actual raster regression checks."""

import importlib.util
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
import xml.etree.ElementTree as ET

from solid_engine import Renderer, Sources, over, solid


ROOT = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("generate_icons", ROOT / "generate.py")
generator = importlib.util.module_from_spec(spec)
spec.loader.exec_module(generator)
RESVG = os.environ.get("RESVG") or shutil.which("resvg")
CONVERT = shutil.which("convert")


class SourceChecks(unittest.TestCase):
    def test_generated_files_and_ui_coverage(self):
        sources, recipes, generated = generator.outputs()
        generator.check_usage({'outline': generator.outline_outputs(sources),
                               'filled': {f'{name}.svg' for name in recipes}}, ROOT.parents[1])
        self.assertEqual(set(generated), {p.stem for p in (ROOT / 'filled').glob('*.svg')})
        for name, content in generated.items():
            self.assertEqual(content, (ROOT / 'filled' / f'{name}.svg').read_text(), name)
        outlines = generator.outline_outputs(sources)
        self.assertEqual(set(outlines), {p.name for p in (ROOT / 'outline').glob('*.svg')})
        for name, content in outlines.items():
            self.assertEqual(content, (ROOT / 'outline' / name).read_text(), name)
            display = ET.fromstring(content)
            self.assertAlmostEqual(float(display.get('stroke-width')) * 16 / 24, 1.2)
            display.set('stroke-width', '2')
            original = sources.trees[sources.manifest['outlines'][name]['source']]
            self.assertEqual(ET.tostring(display), ET.tostring(original), name)

    def test_source_drift_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            target = Path(directory) / 'lucide'
            shutil.copytree(ROOT, target)
            with (target / 'sources' / 'square-pen.svg').open('a') as output:
                output.write('\n')
            with self.assertRaisesRegex(ValueError, 'Source changed:'):
                Sources(target)

    def test_missing_ui_asset_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            ui = Path(directory)
            for family in ('filled', 'outline'):
                with self.subTest(family=family):
                    (ui / 'example.slint').write_text(f'source: @image-url("icons/lucide/{family}/new-icon.svg");')
                    with self.assertRaisesRegex(ValueError, f'Missing generated UI assets.*{family}/new-icon'):
                        generator.check_usage({}, ui)

    def test_check_does_not_modify_assets(self):
        before = {p: (p.read_bytes(), p.stat().st_mtime_ns)
                  for family in ('outline', 'filled') for p in (ROOT / family).glob('*.svg')}
        subprocess.run([sys.executable, str(ROOT / 'generate.py'), '--check'], check=True,
                       stdout=subprocess.PIPE)
        self.assertEqual(before, {p: (p.read_bytes(), p.stat().st_mtime_ns) for p in before})

@unittest.skipUnless(RESVG and CONVERT, 'Raster tests require RESVG and ImageMagick convert')
class RasterChecks(unittest.TestCase):
    def alpha(self, svg, size=240):
        png = subprocess.run([RESVG, '--width', str(size), '--height', str(size), '-', '-c'],
                             input=svg.encode(), stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=True).stdout
        raw = subprocess.run([CONVERT, 'png:-', '-alpha', 'extract', '-depth', '8', 'gray:-'],
                             input=png, stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=True).stdout
        self.assertEqual(len(raw), size * size)
        return raw

    def test_layer_separates_without_erasing_foreground(self):
        back = solid(ET.Element('rect', x='2', y='2', width='16', height='18'))
        front = solid(ET.Element('circle', cx='17', cy='12', r='4'))
        alpha = self.alpha(Renderer().svg(over(back, front)))
        self.assertGreater(alpha[120 * 240 + 50], 250)  # background remains solid
        self.assertLess(alpha[120 * 240 + 120], 5)      # 1.5-unit separation
        self.assertGreater(alpha[120 * 240 + 170], 250) # foreground inside body
        self.assertGreater(alpha[120 * 240 + 200], 250) # foreground outside body

    def test_compose_body_gap_and_exterior_pencil(self):
        svg = (ROOT / 'filled' / 'compose.svg').read_text()
        alpha = self.alpha(svg, 192)
        self.assertGreater(alpha[32 * 192 + 32], 250)   # square
        self.assertLess(alpha[32 * 192 + 116], 5)       # separation
        self.assertGreater(alpha[32 * 192 + 180], 250)  # pencil past square edge

    def test_send_line_and_warning_cutouts(self):
        for name, hole, body in (
            ('paper-airplane', (15, 9), (8, 8)),
            ('alert', (12, 11), (8, 17)),
        ):
            alpha = self.alpha((ROOT / 'filled' / f'{name}.svg').read_text())
            self.assertLess(alpha[hole[1] * 10 * 240 + hole[0] * 10], 20, name)
            self.assertGreater(alpha[body[1] * 10 * 240 + body[0] * 10], 250, name)

    def test_open_folder_has_no_stray_rear_corner(self):
        svg = (ROOT / 'filled' / 'folder-open.svg').read_text()
        alpha = self.alpha(svg)
        # The sloping flap ends left of this strip. A wider closed-folder back
        # used to leave a detached fragment here after applying its clearance.
        corner = [alpha[y * 240 + x] for y in range(175, 195) for x in range(213, 225)]
        self.assertLess(max(corner), 5)
        self.assertGreater(alpha[80 * 240 + 40], 250)  # rear panel retained
        self.assertGreater(alpha[160 * 240 + 150], 250)  # solid front flap

    def test_every_icon_renders_at_native_sizes(self):
        for path in sorted((ROOT / 'filled').glob('*.svg')):
            for size in (16, 24):
                alpha = self.alpha(path.read_text(), size)
                self.assertGreater(sum(alpha), 255 * 5, (path.name, size))
                self.assertIn(0, alpha, (path.name, size))


if __name__ == '__main__':
    unittest.main()
