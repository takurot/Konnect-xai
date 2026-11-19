"""Accept only the netlist, reports and CAM data this two-net fixture expects."""

import json
import re
import xml.etree.ElementTree as ET
from collections import Counter
from itertools import pairwise
from pathlib import Path

ROUTES = {
    "LOAD_A": [(112, 108), (112, 110), (114.54, 112.54), (114.54, 116)],
    "LOAD_B": [(119.62, 108), (119.62, 110), (117.08, 112.54), (117.08, 116)],
}
OUTLINE = [(106, 102), (126, 102), (126, 122), (106, 122), (106, 102)]
LAYERS = {
    "F.Cu": "Copper,L1,Top",
    "B.Cu": "Copper,L2,Bot",
    "F.Mask": "Soldermask,Top",
    "B.Mask": "Soldermask,Bot",
    "F.SilkS": "Legend,Top",
    "B.SilkS": "Legend,Bot",
    "Edge.Cuts": "Profile,NP",
}


def require(condition, error):
    if not condition:
        raise ValueError(error)


def target_ready(response, target):
    """Poll without writing while the endpoint has no board, refuse other boards."""
    requested = response.get("requested_board")
    boards = response.get("open_boards")
    require(
        isinstance(requested, str) and Path(requested).resolve() == target,
        f"IPC resolved wrong requested board: {response}",
    )
    require(
        isinstance(boards, list) and all(isinstance(board, str) for board in boards),
        f"IPC open board inventory is unavailable: {response}",
    )
    if not boards:
        require(
            response.get("requested_open") in (None, False),
            f"IPC reports an open target without an open board inventory: {response}",
        )
        return False
    require(
        len(boards) == 1 and Path(boards[0]).resolve() == target,
        f"IPC has another open board: {response}",
    )
    return response.get("requested_open") is True


def verify_netlist(contents):
    expected = {
        "LOAD_A": [("J1", "1"), ("R1", "1")],
        "LOAD_B": [("J1", "2"), ("R1", "2")],
    }
    observed = {}
    for net in ET.fromstring(contents).findall("./nets/net"):
        name = net.get("name", "").removeprefix("/")
        require(name not in observed, f"duplicate netlist net {name!r}")
        observed[name] = sorted(
            (node.attrib["ref"], node.attrib["pin"]) for node in net.findall("node")
        )
    require(observed == expected, f"KiCad netlist differs: {observed}")
    return observed


def verify_drc(summary, report, *, omitted_pads):
    """Return whether export is authorized, or raise on incomplete/bad evidence."""
    omitted = 0 if omitted_pads is None else 1
    counts = {
        "design_rule_violations": 0,
        "unconnected_items": omitted,
        "schematic_parity": 0,
        "errors": omitted,
        "warnings": 0,
        "total_violations": omitted,
        "filtered_count": omitted,
        "shown": omitted,
    }
    for key, expected in counts.items():
        value = summary.get(key)
        require(
            type(value) is int and value == expected,
            f"DRC {key}: {value!r}, expected {expected}",
        )
    require(
        summary.get("categories_not_reported") == [],
        "DRC categories_not_reported must be empty",
    )
    require(
        summary.get("source") == "saved_file"
        and summary.get("live_board_synced") is True,
        "DRC did not verify the saved live board",
    )
    require(
        summary.get("severity_filter") == "info" and summary.get("truncated") is False,
        "DRC report was filtered or truncated",
    )
    for key, expected in (
        ("violations", 0),
        ("schematic_parity", 0),
        ("unconnected_items", omitted),
    ):
        records = report.get(key)
        require(
            isinstance(records, list) and len(records) == expected,
            f"DRC report {key}: expected {expected} entries, got {records}",
        )
    if omitted:
        result = report["unconnected_items"][0]
        require(
            isinstance(result, dict)
            and result.get("rule") == "unconnected_items"
            and result.get("severity") == "error",
            f"unexpected unconnected rule: {result}",
        )
        items = result.get("items")
        require(
            isinstance(items, list) and len(items) == len(omitted_pads),
            f"wrong unconnected pad count: {items}",
        )
        actual = {}
        for item in items:
            owner = item.get("owner") or {}
            require(
                item.get("item_kind") == "pad"
                and item.get("ownership_status") == "resolved"
                and owner.get("kind") == "footprint",
                f"unresolved unconnected pad: {item}",
            )
            actual[item.get("uuid")] = owner.get("reference")
        require(actual == omitted_pads, f"unconnected pads differ: {actual}")
    require(
        summary.get("violations") == report["unconnected_items"],
        "DRC summary and saved report disagree",
    )
    return omitted_pads is None


def canonical_segment(a, b):
    return tuple(sorted((a, b)))


def segments(points):
    return [canonical_segment(a, b) for a, b in pairwise(points)]


def gerber_geometry(text, layer):
    """Read stock KiCad's absolute mm, 4.6-format linear geometry for this case."""
    require(
        "%FSLAX46Y46*%" in text and "%MOMM*%" in text,
        f"{layer}: unsupported Gerber format",
    )
    moves, flashes = [], []
    prior = net = selected = None
    apertures = set()
    in_macro = False
    lines = text.splitlines()
    require(
        lines and lines[-1] == "M02*" and lines.count("M02*") == 1,
        f"{layer}: expected one Gerber end command",
    )
    for line in lines:
        if in_macro:
            require(
                re.fullmatch(r"(?:0 .+|[1-9][0-9]*,[0-9,$+./xX -]+)\*%?", line),
                f"{layer}: malformed aperture macro line {line!r}",
            )
            in_macro = not line.endswith("%")
            continue
        if re.fullmatch(r"%AM[A-Za-z0-9_]+\*", line):
            in_macro = True
            continue
        aperture = re.fullmatch(r"%ADD([0-9]+)[A-Za-z][^*]*\*%", line)
        if aperture:
            require(
                aperture[1] not in apertures,
                f"{layer}: duplicate aperture D{aperture[1]}",
            )
            apertures.add(aperture[1])
            continue
        selection = re.fullmatch(r"D(\d+)\*", line)
        if selection:
            require(selection[1] in apertures, f"{layer}: undefined aperture {line!r}")
            selected = selection[1]
            continue
        point = re.fullmatch(r"X(-?\d+)Y(-?\d+)D0([123])\*", line)
        if point:
            x, y, op = point.groups()
            position = int(x) / 1e6, -int(y) / 1e6
            if op in ("1", "3"):
                require(selected is not None, f"{layer}: missing aperture for {line!r}")
            if op == "1":
                require(prior is not None, f"{layer}: draw without a prior position")
                moves.append((net, canonical_segment(prior, position)))
            if op == "3":
                flashes.append(position)
            prior = position
        elif line.startswith("%TO.N,"):
            match = re.fullmatch(r"%TO\.N,([^*]+)\*%", line)
            require(match is not None, f"{layer}: malformed net attribute {line}")
            net = match[1].removeprefix("/")
        elif line in ("%TD*%", "%TD.N*%"):
            net = None
        else:
            require(
                not line
                or re.fullmatch(
                    r"(?:%T[AFOD]\.[^*]+\*%|%FSLAX46Y46\*%|%MOMM\*%|%LPD\*%|G04.*\*|G01\*|M02\*)",
                    line,
                ),
                f"{layer}: unsupported Gerber line {line!r}",
            )
    require(not in_macro, f"{layer}: unterminated aperture macro")
    return moves, flashes


def drill_geometry(text):
    lines = text.splitlines()
    functions = [
        line.removeprefix("; #@! TF.FileFunction,")
        for line in lines
        if line.startswith("; #@! TF.FileFunction,")
    ]
    require(
        functions in (["Plated,1,2,PTH"], ["NonPlated,1,2,NPTH"]),
        "invalid Excellon drill plating",
    )
    require(
        lines[0] == "M48" and lines[-1] == "M30" and lines.count("%") == 1,
        "unsupported Excellon drill format",
    )
    delimiter = lines.index("%")
    require(
        "METRIC" in lines[:delimiter]
        and "; FORMAT={-:-/ absolute / metric / decimal}" in lines[:delimiter]
        and lines[delimiter + 1 : delimiter + 3] == ["G90", "G05"],
        "unsupported Excellon drill coordinates or units",
    )
    tools = set()
    for line in lines[1:delimiter]:
        if line.startswith(";") or line in ("", "FMAT,2", "METRIC"):
            continue
        tool = re.fullmatch(r"(T\d+)C(\d+(?:\.\d+)?)", line)
        require(
            tool is not None and float(tool[2]) > 0 and tool[1] not in tools,
            f"invalid drill tool definition {line!r}",
        )
        tools.add(tool[1])
    holes = []
    selected = None
    for line in lines[delimiter + 3 : -1]:
        if not line:
            continue
        if line in tools:
            selected = line
            continue
        hole = re.fullmatch(r"X(-?\d+(?:\.\d+)?)Y(-?\d+(?:\.\d+)?)", line)
        require(
            hole is not None and selected is not None,
            f"unsupported Excellon drill line {line!r}",
        )
        holes.append((float(hole[1]), -float(hole[2])))
    return functions[0], holes


def verify_fabrication(directory, exported):
    files = {path.name: path for path in directory.iterdir()}
    require(
        isinstance(exported, list)
        and all(isinstance(name, str) for name in exported)
        and len(exported) == len(files)
        and set(exported) == files.keys(),
        "exported file list differs from output directory",
    )
    require(
        all(
            path.is_file() and not path.is_symlink() and path.stat().st_size > 0
            for path in files.values()
        ),
        "manufacturing files must be nonempty regular files",
    )
    jobs = [path for path in files.values() if path.suffix == ".gbrjob"]
    require(len(jobs) == 1, "expected one Gerber job manifest")

    functions = {
        function.lower().removesuffix(",np"): layer
        for layer, function in LAYERS.items()
    }
    plots, names = {}, {}
    for record in json.loads(jobs[0].read_text(encoding="utf-8"))["FilesAttributes"]:
        name = record["Path"]
        require(
            name in files and name not in names.values(),
            f"invalid Gerber manifest file {name!r}",
        )
        contents = files[name].read_text(encoding="utf-8")
        attr = re.findall(r"^%TF\.FileFunction,([^*]+)\*%$", contents, re.MULTILINE)
        function = record["FileFunction"].lower().removesuffix(",np")
        require(
            len(attr) == 1
            and attr[0].lower().removesuffix(",np") == function
            and function in functions,
            f"Gerber manifest layer does not match {name}",
        )
        layer = functions[function]
        require(
            layer not in plots and contents.rstrip().endswith("M02*"),
            f"duplicated or incomplete Gerber layer {layer}",
        )
        names[layer] = name
        plots[layer] = contents
    require(plots.keys() == LAYERS.keys(), f"missing Gerber layers: {plots.keys()}")

    drill_files = set(files) - set(names.values()) - {jobs[0].name}
    drills = {}
    for name in sorted(drill_files):
        require(files[name].suffix == ".drl", f"unexpected fabrication file {name}")
        function, hits = drill_geometry(files[name].read_text(encoding="utf-8"))
        require(function not in drills, f"duplicate Excellon drill function {function}")
        drills[function] = hits
    holes = sorted(points[index] for points in ROUTES.values() for index in (0, -1))
    require(
        sorted(drills.get("Plated,1,2,PTH", [])) == holes
        and not drills.get("NonPlated,1,2,NPTH", []),
        "Excellon drill inventory differs",
    )

    for layer in ("F.Cu", "B.Cu", "F.Mask", "B.Mask"):
        moves, flashes = gerber_geometry(plots[layer], layer)
        require(
            sorted(flashes) == holes, f"{layer}: flashes do not match all PTH holes"
        )
        expected = [
            (net, move) for net, points in ROUTES.items() for move in segments(points)
        ]
        require(
            Counter(moves) == Counter(expected if layer == "F.Cu" else ()),
            f"{layer}: tracks differ from the fixture",
        )
    edges, flashes = gerber_geometry(plots["Edge.Cuts"], "Edge.Cuts")
    require(
        not flashes
        and Counter(edges) == Counter((None, edge) for edge in segments(OUTLINE)),
        "Edge.Cuts: expected one closed 20 mm square",
    )
    return {
        "layers": names,
        "pth_holes": holes,
        "front_segments_per_net": {
            net: len(points) - 1 for net, points in ROUTES.items()
        },
        "back_track_count": 0,
        "outline_mm": [20, 20],
    }
