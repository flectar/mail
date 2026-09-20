"""Filled icon recipes built from hash-pinned SVG elements or custom paths.

Element indices refer to zero-based direct SVG children. Recipes define contour
closures, path splits and optical adjustments.
"""

from dataclasses import dataclass
import xml.etree.ElementTree as ET
from solid_engine import ACTIVE_WIDTH, OUTLINE_WIDTH, cut, group, line, over, solid


@dataclass(frozen=True)
class Recipe:
    source: str | None  # None identifies a fully custom drawing.
    drawing: object
    note: str


def build(sources):
    p = sources.part

    def lines(name, *indices, width=2):
        return group(*(line(p(name, i), width) for i in indices))

    def calendar():
        return group(cut(solid(p("calendar-days", 2)), line(p("calendar-days", 3))),
                     lines("calendar-days", 0, 1))

    # Square-pen's interrupted frame has the same bounds as calendar-days' rect.
    compose = over(solid(p("calendar-days", 2), OUTLINE_WIDTH),
                   solid(p("square-pen", 1), OUTLINE_WIDTH))
    # File supplies the complete contour missing underneath file-pen-line's pen.
    page = cut(solid(p("file", 0)), line(p("file", 1), 1.5))
    draft = over(page, solid(p("file-pen-line", 0), OUTLINE_WIDTH))

    # Close the hidden rear page. Front path ends join across its folded corner;
    # retain the original fold curve and translate file's seam by one unit.
    back_page = solid(p("files", 2, append="V7Z"))
    front_page = cut(group(solid(p("files", 0, append="Z")), solid(p("files", 1))),
                     line(p("file", 1, attrs={"transform": "translate(1 0)"}), 1.5))
    # Folder-open combines both sides in one open path. Split at the front's
    # bottom-left corner. Continue from that same point for the rear panel;
    # a closed-folder substitute is wider and leaves a stray lower-right sliver.
    open_front = solid(p("folder-open", 0, until="H4", append="Z"))
    open_back = solid(p("folder-open", 0, after="H4", move_to=(4, 20), append="V20Z"))

    # Optical correction: space between a negative head and shoulders at 16px.
    # Close the shoulder arc along its bottom.
    def portrait(name, head, shoulders):
        return group(solid(p(name, head, attrs={"cy": 10, "r": 3.5})),
                     solid(p(name, shoulders, append="Z")))

    person = group(solid(p("users", 3)), solid(p("users", 0, append="Z")))
    # Custom front-facing tray, following the Things reference: the sides lean
    # inward by only 0.35 units toward the top. The opening follows that taper,
    # with broad curved shoulders and softly rounded bottom corners. A slightly
    # thinner top rim keeps the opening airy at the 18px sidebar display size.
    inbox_flectar = cut(solid(ET.Element("path", d=(
        "M4.75 2H19.25C20.55 2 21.62 3.02 21.65 4.32L22 19.32"
        "C22.035 20.78 20.91 22 19.45 22H4.55"
        "C3.09 22 1.965 20.78 2 19.32L2.35 4.32"
        "C2.38 3.02 3.45 2 4.75 2Z"
    ))), solid(ET.Element("path", d=(
        "M5.8 5.4H18.2C18.61 5.4 18.935 5.72 18.95 6.13L19.1 11.6"
        "C19.115 12.1 18.73 12.5 18.23 12.5H17.5"
        "A2.25 2.25 0 0 0 15.25 14.75V18.75"
        "A.75 .75 0 0 1 14.5 19.5H9.5A.75 .75 0 0 1 8.75 18.75V14.75"
        "A2.25 2.25 0 0 0 6.5 12.5H5.77"
        "C5.27 12.5 4.885 12.1 4.9 11.6L5.05 6.13"
        "C5.065 5.72 5.39 5.4 5.8 5.4Z"
    ))))
    # Preserve the earlier opening as a generated variant, byte-for-byte.
    inbox_flectar_old = cut(inbox_flectar.body, solid(ET.Element("path", d=(
        "M5.95 5.75H18.05C18.53 5.75 18.935 6.12 18.95 6.6L19.1 11.6"
        "C19.115 12.1 18.73 12.5 18.23 12.5H17.5"
        "A2.25 2.25 0 0 0 15.25 14.75V18.75"
        "A.75 .75 0 0 1 14.5 19.5H9.5A.75 .75 0 0 1 8.75 18.75V14.75"
        "A2.25 2.25 0 0 0 6.5 12.5H5.77"
        "C5.27 12.5 4.885 12.1 4.9 11.6L5.05 6.6"
        "C5.065 6.12 5.47 5.75 5.95 5.75Z"
    ))))
    recipes = {
        "account": Recipe("circle-user-round", cut(
            solid(p("circle-user-round", 2)), portrait("circle-user-round", 1, 0)),
            "Solid circle; portrait cutout with optical head spacing."),
        "alert": Recipe("triangle-alert", cut(
            solid(p("triangle-alert", 0)), lines("triangle-alert", 1, 2)),
            "Solid warning triangle; exclamation cutout."),
        "archive": Recipe("archive", over(
            cut(solid(p("archive", 1, append="Z")), line(p("archive", 2))),
            solid(p("archive", 0)), gap=1),
            "Close the box rim; separate solid lid; handle cutout."),
        "bell": Recipe("bell", group(solid(p("bell", 1)), line(p("bell", 0))),
            "Solid bell with the original clapper stroke."),
        "calendar": Recipe("calendar-days", cut(calendar(), lines("calendar-days", *range(4, 10))),
            "Solid calendar; header seam and date cutouts; binding strokes."),
        "calendar-add": Recipe("calendar-plus", over(calendar(), lines("calendar-plus", 0, 2)),
            "Complete calendar body with separated exterior plus badge."),
        "compose": Recipe("square-pen", compose,
            "Solid square and pencil with transparent separation."),
        "contacts": Recipe("contact-round", group(
            cut(solid(p("contact-round", 4)), portrait("contact-round", 3, 1)),
            lines("contact-round", 0, 2)),
            "Solid address book; portrait cutout and original binding strokes."),
        "database": Recipe("database", cut(
            group(solid(p("database", 0)), solid(p("database", 1, append="Z"))),
            # Divider translated up seven units traces the lid's lower half.
            line(p("database", 2, attrs={"transform": "translate(0 -7)"}), 1.5),
            line(p("database", 2), 1.5)),
            "Solid cylinder; two curved seams from the upstream divider."),
        "draft": Recipe("file-pen-line", draft,
            "Complete solid file; fold cutout; separated solid exterior pencil."),
        "file-directory": Recipe("folder", solid(p("folder", 0)),
            "Original closed folder silhouette."),
        "files": Recipe("files", over(back_page, front_page),
            "Two solid pages in original order; separation and fold cutout."),
        "folder-open": Recipe("folder-open", over(open_back, open_front),
            "Separate front and rear panels from the same original open-folder contour."),
        "folders": Recipe("folders", over(
            solid(p("folders", 1, append="V8.268Z")), solid(p("folders", 0))),
            "Close hidden rear folder; separate original solid foreground folder."),
        "gear": Recipe("settings", cut(solid(p("settings", 0)), solid(p("settings", 1))),
            "Solid gear; central hole."),
        "inbox": Recipe("inbox", cut(solid(p("inbox", 1)), line(p("inbox", 0), 1.8)),
            "Solid tray; original lip becomes a cutout."),
        "inbox-flectar": Recipe(None, inbox_flectar,
            "Custom tapered tray; softly rounded outline and curved shoulders around an open well."),
        "inbox-flectar-old": Recipe(None, inbox_flectar_old,
            "Preserved earlier Flectar tray opening."),
        "info": Recipe("info", cut(solid(p("info", 0)), lines("info", 1, 2)),
            "Solid circle; information mark cutout."),
        "keyboard": Recipe("keyboard", cut(solid(p("keyboard", 8)), lines("keyboard", *range(8))),
            "Solid keyboard; original keys and space bar become cutouts."),
        "mail": Recipe("mail", cut(solid(p("mail", 1)), line(p("mail", 0))),
            "Solid envelope; original flap seam cutout."),
        "mail-open": Recipe("mail-open", cut(solid(p("mail-open", 0)), line(p("mail-open", 1))),
            "Solid open envelope; original flap seam cutout."),
        "paper-airplane": Recipe("send", cut(solid(p("send", 0)), line(p("send", 1), 1.75)),
            "Solid plane; center line is transparent so it remains visible."),
        "people": Recipe("users", over(
            # Upstream's rear person contains only exposed arcs. Complete it
            # from the same icon's full head/torso instead of filling arc chords.
            group(solid(p("users", 3, attrs={"cx": 17, "r": 3})),
                  solid(p("users", 0, append="Z", attrs={"transform": "translate(6 0)"}))), person),
            "Complete rear person from sibling shapes; separate solid foreground person."),
        "person-add": Recipe("user-plus", over(
            group(solid(p("user-plus", 1)), solid(p("user-plus", 0, append="Z"))),
            lines("user-plus", 2, 3, width=2.7)),
            "Solid person and exterior plus, with automatic separation."),
        "sliders": Recipe("sliders-horizontal", lines("sliders-horizontal", *range(9), width=ACTIVE_WIDTH),
            "Original line-based controls at active weight."),
        "stack": Recipe("layers-3", group(solid(p("layers-3", 0)), lines("layers-3", 1, 2, width=2.5)),
            "Solid top layer; original lower layer contours at active weight."),
        "star": Recipe("star", solid(p("star", 0)), "Original closed star silhouette."),
        "tag": Recipe("tag", cut(solid(p("tag", 0)), solid(p("tag", 1), width=2)),
            "Solid tag; hole includes the original circle's stroke footprint."),
        "trash": Recipe("trash-2", over(
            cut(solid(p("trash-2", 2, append="Z")), lines("trash-2", 0, 1, width=1.6)),
            lines("trash-2", 3, 4)),
            "Close solid bin; slot cutouts; separate lid and handle."),
    }
    for filename, source in {
        "bold": "bold", "code": "code-xml", "italic": "italic",
        "link": "link-2", "paperclip": "paperclip", "screen-full": "maximize",
        "search": "search", "strikethrough": "strikethrough", "typography": "underline",
    }.items():
        recipes[filename] = Recipe(source, lines(source, *range(len(sources.trees[source])), width=ACTIVE_WIDTH),
                                   "Original line geometry at active weight; no meaningful enclosed fill.")
    return recipes
