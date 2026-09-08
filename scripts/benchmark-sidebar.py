#!/usr/bin/env python3
"""Render synthetic folder trees and count constructed sidebar delegates.

Requires slint-viewer 1.17+. Use --baseline-ref with a pre-virtualization
commit to compare the original ScrollView. Timings include interpretation, layout, and one software frame;
they are diagnostic, not a compiled application or hardware benchmark.
"""
import argparse
import json
from pathlib import Path
import re
import statistics
import subprocess
import tempfile
import time

REPO = Path(__file__).resolve().parents[1]


def mailbox(account_id, folder=None):
    return {
        "account_id": account_id, "folder_id": folder if folder is not None else -1,
        "parent_folder_id": -1, "depth": 0, "has_children": False, "expanded": True,
        "is_standard": False, "label_has_emoji": False,
        "label": f"Folder {folder}" if folder is not None else f"Account {account_id}",
        "scope": f"Folder:{folder}" if folder is not None else f"Account:{account_id}",
        "context": f"Account:{account_id}", "detail": "", "avatar": "A",
        "has_avatar": False, "is_account": folder is None, "count": "",
        "color": "#000000", "has_custom_color": False,
    }


def fixture(account_count, folder_count, flat):
    accounts = [mailbox(i) for i in range(account_count)]
    folders = [mailbox(i, i * folder_count + f) for i in range(account_count) for f in range(folder_count)]
    if not flat:
        return {"account-mailboxes": accounts, "mailboxes": accounts + folders}
    label = dict(id=-1, name="", name_has_emoji=False, color="#000000", applied=False, is_auto=False)

    def row(kind, key, data=None, opened=False):
        return dict(kind=kind, key=key, mailbox=data or mailbox(-1), label=label, open=opened)

    rows = [row("unified-heading", "heading"), row("unified-section", "unified", opened=True),
            row("unified-inbox", "inbox"), row("categories-section", "categories"),
            row("accounts-heading", "accounts")]
    for account in accounts:
        aid = account["account_id"]
        rows.append(row("account", f"account:{aid}", account, True))
        rows.extend(row("folder", f"folder:{f['folder_id']}", f) for f in folders if f["account_id"] == aid)
        rows.append(row("new-folder", f"new:{aid}", account))
    rows.append(row("add-account", "add"))
    return {"rows": rows}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline-ref")
    parser.add_argument("--rounds", type=int, default=3)
    parser.add_argument("--output-dir", type=Path, default=Path("/tmp/flectar-sidebar-benchmark"))
    args = parser.parse_args()
    args.output_dir.mkdir(parents=True, exist_ok=True)
    path = "ui/components/sidebar.slint"
    source = subprocess.check_output(["git", "show", f"{args.baseline_ref}:{path}"], cwd=REPO, text=True) if args.baseline_ref else (REPO / path).read_text()
    flat = "[SidebarRow]" in source
    for component in ("SidebarItem", "SidebarSectionHeader"):
        source = source.replace(f"export component {component} inherits Rectangle {{",
                                f'export component {component} inherits Rectangle {{\n    init => {{ debug("sidebar-delegate-created"); }}')
    source = re.sub(r'"((?:\.\./|\./)[^"\n]+)"', lambda m: json.dumps(str((REPO / "ui/components" / m[1]).resolve())), source)
    with tempfile.TemporaryDirectory(prefix="sidebar-bench-") as directory:
        directory = Path(directory)
        if not args.baseline_ref and (REPO / "ui/components/sidebar-controls.slint").exists():
            controls = (REPO / "ui/components/sidebar-controls.slint").read_text()
            for component in ("SidebarItem", "SidebarSectionHeader"):
                controls = controls.replace(f"export component {component} inherits SidebarRowSurface {{",
                                            f'export component {component} inherits SidebarRowSurface {{\n    init => {{ debug("sidebar-delegate-created"); }}')
            controls = re.sub(r'"((?:\.\./|\./)[^"\n]+)"', lambda m: json.dumps(str((REPO / "ui/components" / m[1]).resolve())), controls)
            (directory / "sidebar-controls.slint").write_text(controls)
        (directory / "sidebar.slint").write_text(source)
        model_type = "SidebarRow" if flat else "MailboxRow"
        account_property = "" if flat else "in property <[MailboxRow]> account-mailboxes;"
        binding = ("rows: root.rows;" if flat else
                   "mailboxes: root.rows; account-mailboxes: root.account-mailboxes;")
        (directory / "main.slint").write_text(f'''import {{ {model_type} }} from "{REPO / 'ui/models.slint'}";
import {{ MailSidebar }} from "sidebar.slint";
export component Harness inherits Window {{
    width: 240px; height: 800px;
    in property <[{model_type}]> rows;
    {account_property}
    MailSidebar {{
        {binding}
        background-color: #f4f4f4; body-color: #222; title-color: #222;
        muted-color: #777; accent-color: #06e;
        selection-background: #dfecff; hover-background: #ddd;
    }}
}}
''')
        for account_count, folder_count in [(1, 750), (5, 150), (5, 1500)]:
            data = fixture(account_count, folder_count, flat)
            if not flat:
                data["rows"] = data.pop("mailboxes")
            (directory / "data.json").write_text(json.dumps(data))
            times, counts = [], []
            output = args.output_dir / f"{'flat' if flat else 'baseline'}-{account_count}x{folder_count}.png"
            for _ in range(args.rounds):
                started = time.perf_counter()
                result = subprocess.run(["slint-viewer", "-I", str(REPO / "ui/components"), "--style", "fluent", "--load-data", str(directory / "data.json"),
                                         "--screenshot", str(output), str(directory / "main.slint")], capture_output=True, text=True)
                if result.returncode:
                    raise RuntimeError(result.stdout + result.stderr)
                times.append(time.perf_counter() - started)
                counts.append((result.stdout + result.stderr).count("sidebar-delegate-created"))
                if "Warning:" in result.stderr:
                    raise RuntimeError(result.stderr)
            assert min(counts) > 0, "instrumentation must observe instantiated delegates"
            if flat:
                assert max(counts) < 100, f"virtualization regression: {counts} delegates"
            print(f"accounts={account_count} folders_each={folder_count} delegates={counts} median_seconds={statistics.median(times):.4f} screenshot={output}", flush=True)


if __name__ == "__main__":
    main()
