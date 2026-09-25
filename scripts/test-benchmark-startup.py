#!/usr/bin/env python3
"""Checks for anonymized Linux mapping attribution in the startup benchmark."""

import importlib.util
import json
import unittest
from pathlib import Path


script = Path(__file__).with_name("benchmark-startup.py")
spec = importlib.util.spec_from_file_location("benchmark_startup", script)
benchmark = importlib.util.module_from_spec(spec)
spec.loader.exec_module(benchmark)


class SmapsParsingTests(unittest.TestCase):
    def test_mappings_are_counted_without_profile_paths(self):
        smaps = """\
55550000-55551000 r-xp 00000000 08:01 10 /opt/flectar-mail
Size:                  4 kB
Rss:                   4 kB
Pss:                   4 kB
Private_Dirty:         0 kB
Swap:                  0 kB
SwapPss:               0 kB
55551000-55553000 rw-p 00000000 08:01 11 /private/profile/mail.db
Size:                  8 kB
Rss:                   8 kB
Pss:                   6 kB
Private_Dirty:         4 kB
Swap:                  2 kB
SwapPss:               2 kB
55553000-55554000 rw-p 00000000 00:00 0 [heap]
Size:                  4 kB
Rss:                   4 kB
Pss:                   4 kB
Private_Dirty:         4 kB
Swap:                  0 kB
SwapPss:               0 kB
"""
        report = benchmark.parse_smaps(smaps, Path("/opt/flectar-mail"))

        self.assertEqual(report["mapping_count"], 3)
        self.assertEqual(report["by_kind"]["database_file"]["pss_kib"], 6)
        self.assertEqual(report["by_kind"]["database_file"]["swap_pss_kib"], 2)
        self.assertEqual(report["by_kind"]["heap"]["private_dirty_kib"], 4)
        self.assertEqual(report["by_kind"]["executable"]["rss_kib"], 4)
        self.assertEqual(
            sum(group["pss_kib"] for group in report["by_kind"].values()), 14
        )
        self.assertNotIn("/private/profile", json.dumps(report))

    def test_renderer_selection_is_recorded_for_cpu_gate(self):
        events, tail = {}, []
        benchmark.record_stderr_line(
            'FLECTAR_RENDERER {"event":"selected","active":"cpu","wgpu_initialized":false}',
            events,
            tail,
        )
        self.assertEqual(events["renderer_selected"]["active"], "cpu")
        self.assertIs(events["renderer_selected"]["wgpu_initialized"], False)
        self.assertEqual(tail, [])


if __name__ == "__main__":
    unittest.main()
