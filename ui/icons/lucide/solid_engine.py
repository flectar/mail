"""SVG composition primitives for source-derived icon recipes."""

from copy import deepcopy
from dataclasses import dataclass
import hashlib
import json
from pathlib import Path
import xml.etree.ElementTree as ET


OUTLINE_WIDTH = 2.0  # Original footprint used by solid recipes, not display styling.
DISPLAY_SIZE = 16
DISPLAY_STROKE_PX = 1.2
DISPLAY_OUTLINE_WIDTH = DISPLAY_STROKE_PX * 24 / DISPLAY_SIZE
ACTIVE_WIDTH = 3.0
DETAIL_WIDTH = 2.0
SEPARATION = 1.5  # 1 CSS/logical pixel when the 24-unit viewBox is shown at 16px.
SVG_NS = "http://www.w3.org/2000/svg"


class Sources:
    """Load source SVGs and verify their hashes against the manifest."""

    def __init__(self, root: Path):
        self.root = root
        self.manifest = json.loads((root / "sources.json").read_text())
        self.trees = {}
        for name, item in self.manifest["sources"].items():
            data = (root / item["path"]).read_bytes()
            self.verify(data, item["sha256"], item["path"])
            tree = ET.fromstring(data)
            if tree.get("viewBox") != "0 0 24 24":
                raise ValueError(f"Unexpected viewBox: {name}")
            self.trees[name] = tree

    def outline(self, name: str):
        """Keep original geometry; match a 1.2px stroke when displayed at 16px."""
        original = (self.root / self.manifest["sources"][name]["path"]).read_text()
        if self.trees[name].get("stroke-width") != "2" or original.count('stroke-width="2"') != 1:
            raise ValueError(f"Unexpected upstream stroke styling: {name}")
        return ('<!-- Generated display outline; originals and license in ../sources/ and ../LICENSE. -->\n'
                + original.replace('stroke-width="2"', f'stroke-width="{DISPLAY_OUTLINE_WIDTH:g}"'))

    @staticmethod
    def verify(data: bytes, expected: str, name: str):
        if hashlib.sha256(data).hexdigest() != expected:
            raise ValueError(f"Source changed: {name}; review recipes before updating its hash")

    def part(self, name: str, index: int, *, append="", until=None, after=None, move_to=None, attrs=None):
        """Select a pinned element; explicit overrides repair interrupted contours.

        `append` closes a contour. `until` splits a compound path at an exact
        command boundary; the pinned hash guards its meaning and child order.
        `after` selects the remaining commands; `move_to` supplies their original
        starting point so relative commands keep their original geometry.
        """
        element = deepcopy(self.trees[name][index])
        element.tag = element.tag.rsplit("}", 1)[-1]
        if element.tag not in {"path", "rect", "circle", "ellipse", "line", "polyline", "polygon"}:
            raise ValueError(f"Unsupported upstream primitive: {name}[{index}]")
        for attr in ("fill", "stroke", "stroke-width", "stroke-linecap", "stroke-linejoin"):
            element.attrib.pop(attr, None)
        if after is not None:
            data = element.attrib["d"]
            if data.count(after) != 1 or move_to is None:
                raise ValueError(f"Ambiguous split or missing start in {name}[{index}]: {after}")
            element.set("d", f"M{move_to[0]:g} {move_to[1]:g} " + data[data.index(after) + len(after):])
        elif move_to is not None:
            raise ValueError("move_to requires an after boundary")
        if until is not None:
            data = element.attrib["d"]
            if data.count(until) != 1:
                raise ValueError(f"Ambiguous split in {name}[{index}]: {until}")
            element.set("d", data[:data.index(until) + len(until)])
        if append:
            element.set("d", element.attrib["d"] + " " + append)
        for key, value in (attrs or {}).items():
            element.set(key, str(value))
        return element


@dataclass(frozen=True)
class Paint:
    shape: ET.Element
    filled: bool
    width: float


@dataclass(frozen=True)
class Group:
    children: tuple


@dataclass(frozen=True)
class Cut:
    body: object
    holes: object


def solid(shape, width=0):
    return Paint(shape, True, width)


def line(shape, width=DETAIL_WIDTH):
    return Paint(shape, False, width)


def group(*children):
    return Group(tuple(children))


def cut(body, *holes):
    """Interior marks are negative space, with no painted background color."""
    return Cut(body, group(*holes))


def halo(node, gap):
    """Expand the full silhouette, including a cut object's exterior."""
    if gap < 0:
        raise ValueError("A separation gap must be nonnegative")
    if isinstance(node, Paint):
        return Paint(node.shape, node.filled, node.width + 2 * gap)
    if isinstance(node, Group):
        return group(*(halo(child, gap) for child in node.children))
    if isinstance(node, Cut):
        return halo(node.body, gap)
    raise TypeError(type(node))


def over(back, front, gap=SEPARATION):
    """Clear only the back object, then draw the solid foreground on top."""
    return group(cut(back, halo(front, gap)), front)


class Renderer:
    def __init__(self):
        self.defs = ET.Element("defs")
        self.counter = 0

    def draw(self, node, color="#000"):
        if isinstance(node, Paint):
            shape = deepcopy(node.shape)
            shape.set("fill", color if node.filled else "none")
            shape.set("stroke", color if node.width else "none")
            if node.width:
                shape.set("stroke-width", f"{node.width:g}")
                shape.set("stroke-linecap", "round")
                shape.set("stroke-linejoin", "round")
            return shape
        if isinstance(node, Group):
            result = ET.Element("g")
            result.extend(self.draw(child, color) for child in node.children)
            return result
        if isinstance(node, Cut):
            self.counter += 1
            mask_id = f"cut-{self.counter}"
            mask = ET.SubElement(self.defs, "mask", {
                "id": mask_id, "maskUnits": "userSpaceOnUse",
                "x": "0", "y": "0", "width": "24", "height": "24",
                "style": "mask-type:luminance",
            })
            ET.SubElement(mask, "rect", {"width": "24", "height": "24", "fill": "white"})
            mask.append(self.draw(node.holes, "black"))
            result = ET.Element("g", {"mask": f"url(#{mask_id})"})
            result.append(self.draw(node.body, color))
            return result
        raise TypeError(type(node))

    def svg(self, node, *, custom=False):
        self.defs = ET.Element("defs")
        self.counter = 0
        root = ET.Element("svg", {
            "xmlns": SVG_NS, "width": "24", "height": "24", "viewBox": "0 0 24 24",
        })
        drawing = self.draw(node)
        if len(self.defs):
            root.append(self.defs)
        root.append(drawing)
        ET.indent(root, space="  ")
        provenance = ('<!-- Generated custom Flectar icon; recipe in ../solid_recipes.py. -->\n'
                      if custom else '<!-- Generated from lucide-static 1.46.0; ISC license in ../LICENSE. -->\n')
        return (provenance
                + ET.tostring(root, encoding="unicode") + "\n")
