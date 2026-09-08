#!/usr/bin/env python3
"""Check bounded contacts/calendar sidebar construction with slint-viewer 1.17+.

Screenshots are written to /tmp/workspace-sidebars. This measures UI delegate
construction, not compiled application timing or hardware performance.
"""
import json
from pathlib import Path
import shutil
import subprocess
import tempfile

REPO = Path(__file__).resolve().parents[1]
OUTPUT = Path("/tmp/workspace-sidebars")
MARKER = "sidebar-delegate-created"


def fixture(workspace, count):
    if workspace == "calendar":
        return [dict(
            id=i, account_id=i // 5, group_start=i % 5 == 0,
            name=f"Calendar {i}", account_name=f"Account {i // 5}",
            email=f"account{i // 5}@example.com", provider="local", color="",
            color_index=i % 4, read_only=False, enabled=True,
            is_default=i % 5 == 0, last_synced="",
        ) for i in range(count)]

    empty = dict(id=-1, name="", email="", initials="", has_avatar=False,
                 count="", selected=False)

    def row(kind, key, account=None):
        return dict(kind=kind, key=key, account=account or empty, open=True)

    rows = [row("unified-heading", "heading"), row("unified-section", "unified"),
            row("all", "all"), row("favorites", "favorites"),
            row("accounts-heading", "accounts")]
    for i in range(count):
        account = dict(empty, id=i, name=f"Account {i}",
                       email=f"account{i}@example.com", initials="AC", count="12")
        rows.extend([row("account", f"account:{i}", account),
                     row("contacts", f"contacts:account:{i}", account)])
    rows.append(row("hint", "hint"))
    return rows


def harness(workspace):
    contacts = workspace == "contacts"
    model = "ContactSidebarRow" if contacts else "CalendarSourceRow"
    component = "ContactsView" if contacts else "FunctionalCalendarSidebar"
    module = "views/contacts-view.slint" if contacts else "components/calendar.slint"
    colors = """
        sidebar-bg: #f4f4f4; surface: #fff; body-text: #222; title-color: #222;
        muted-text: #777; secondary-text: #666; muted-control-bg: #e5e5e5;
        accent: #06e; accent-soft: #dfecff; border: #ddd;
    """ if contacts else """
        surface-color: #f4f4f4; body-color: #222; title-color: #222;
        muted-color: #777; secondary-color: #666; control-hover-color: #e5e5e5;
        accent-color: #06e; accent-background: #dfecff;
    """
    return f'''import {{ {model} }} from "ui/models.slint";
import {{ {component} }} from "ui/{module}";
export component Harness inherits Window {{
    width: {1320 if contacts else 240}px; height: 800px;
    in property <[{model}]> rows;
    in property <bool> active: true;
    in property <bool> collapsed: false;
    {component} {{
        {"sidebar-rows" if contacts else "sources"}: root.rows;
        active: root.active;
        {"sidebar-collapsed" if contacts else "collapsed"}: root.collapsed;
        {colors}
    }}
}}
'''


def main():
    OUTPUT.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="workspace-sidebars-") as directory:
        directory = Path(directory)
        shutil.copytree(REPO / "ui", directory / "ui")
        (directory / "resources").symlink_to(REPO / "resources")
        controls = directory / "ui/components/sidebar-controls.slint"
        source = controls.read_text()
        for name in ["SidebarItem", "SidebarSectionHeader"]:
            declaration = f"export component {name} inherits SidebarRowSurface {{"
            assert declaration in source
            source = source.replace(declaration, declaration + f'\n init => {{ debug("{MARKER}"); }}')
        controls.write_text(source)
        calendar = directory / "ui/components/calendar.slint"
        source = calendar.read_text()
        declaration = "for calendar in root.sources: Rectangle {"
        assert declaration in source
        calendar.write_text(source.replace(declaration, declaration + f'\n init => {{ debug("{MARKER}"); }}'))

        for workspace in ["contacts", "calendar"]:
            (directory / "main.slint").write_text(harness(workspace))
            for count in [750, 7500]:
                rows = fixture(workspace, count)
                for state in ["visible", "inactive", "collapsed"]:
                    data = dict(rows=rows, active=state != "inactive", collapsed=state == "collapsed")
                    (directory / "data.json").write_text(json.dumps(data))
                    screenshot = OUTPUT / f"{workspace}-{count}-{state}.png"
                    result = subprocess.run([
                        "slint-viewer", "--style", "fluent", "--load-data", str(directory / "data.json"),
                        "--screenshot", str(screenshot), str(directory / "main.slint"),
                    ], capture_output=True, text=True, check=True)
                    delegates = (result.stdout + result.stderr).count(MARKER)
                    expected = 0 < delegates < 100 if state == "visible" else delegates == 0
                    assert expected, (workspace, count, state, delegates)
                    print(f"workspace={workspace} rows={count} state={state} delegates={delegates}", flush=True)


if __name__ == "__main__":
    main()
