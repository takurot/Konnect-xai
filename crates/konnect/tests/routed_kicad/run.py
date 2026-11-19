"""Opt-in real KiCad acceptance for one routed board and one unrouted control."""

import argparse
import hashlib
import json
import platform
import re
import subprocess
import time
from contextlib import contextmanager
from datetime import datetime, timezone
from itertools import pairwise
from pathlib import Path
from queue import Empty, Queue
from threading import Thread

from evidence import (
    LAYERS,
    OUTLINE,
    ROUTES,
    canonical_segment,
    require,
    segments,
    target_ready,
    verify_drc,
    verify_drc_source,
    verify_fabrication,
    verify_netlist,
)

CASES = ("routed", "unrouted")
PROJECT = "load-test"
COMPONENTS = {
    "R1": (
        "Device:R",
        "Resistor_THT:R_Axial_DIN0207_L6.3mm_D2.5mm_P7.62mm_Horizontal",
        (101.6, 101.6),
    ),
    "J1": (
        "Connector_Generic:Conn_01x02",
        "Connector_PinHeader_2.54mm:PinHeader_1x02_P2.54mm_Vertical",
        (127, 101.6),
    ),
}
PIN_NETS = {"1": "LOAD_A", "2": "LOAD_B"}


def write_json(path, value):
    path.write_text(
        json.dumps(value, indent=2, ensure_ascii=False) + "\n", encoding="utf-8"
    )


def read_json(path):
    return json.loads(path.read_text(encoding="utf-8"))


def now():
    return datetime.now(timezone.utc).isoformat()


def hashes(paths):
    receipts = {}
    for path in paths:
        require(
            path.is_file() and not path.is_symlink(), f"expected plain file: {path}"
        )
        with path.open("rb") as source:
            receipts[path.name] = hashlib.file_digest(source, "sha256").hexdigest()
    return receipts


def design(root, case):
    folder = root / case
    return tuple(
        folder / f"{PROJECT}.{suffix}"
        for suffix in ("kicad_pcb", "kicad_sch", "kicad_pro")
    )


class Mcp:
    def __init__(self, process, trace):
        self.process = process
        self.trace = trace
        self.next_id = 0
        self.messages = Queue()
        self.called_tools = []
        Thread(target=self.receive, daemon=True).start()

    def receive(self):
        try:
            for line in self.process.stdout:
                self.messages.put(line)
        finally:
            self.messages.put(None)

    def record(self, direction, value):
        self.trace.write(
            json.dumps({"time": now(), direction: value}, ensure_ascii=False) + "\n"
        )
        self.trace.flush()

    def send(self, **message):
        message = dict(message, jsonrpc="2.0")
        self.record("request", message)
        self.process.stdin.write(json.dumps(message) + "\n")
        self.process.stdin.flush()

    def request(self, method, params, timeout=90):
        self.next_id += 1
        self.send(id=self.next_id, method=method, params=params)
        deadline = time.monotonic() + timeout
        while True:
            try:
                line = self.messages.get(timeout=max(0, deadline - time.monotonic()))
            except Empty as error:
                raise TimeoutError(f"MCP request timed out: {method}") from error
            require(line is not None, f"MCP stdin/stdout closed waiting for {method}")
            try:
                response = json.loads(line)
            except ValueError:
                self.record("invalid_stdout", line)
                raise
            self.record("response", response)
            require(
                isinstance(response, dict) and response.get("jsonrpc") == "2.0",
                f"invalid MCP response: {response}",
            )
            if response.get("id") == self.next_id:
                require(
                    "result" in response and "error" not in response,
                    f"MCP request failed: {response}",
                )
                return response["result"]
            require("id" not in response, f"unexpected MCP request ID: {response}")
            if time.monotonic() >= deadline:
                raise TimeoutError(f"MCP request timed out: {method}")

    def tool(self, tool_name, *, timeout=90, **arguments):
        self.called_tools.append(tool_name)
        result = self.request(
            "tools/call", {"name": tool_name, "arguments": arguments}, timeout
        )
        require(result.get("isError") is False, f"{tool_name}: {result}")
        text = result["content"][0]["text"]
        require(isinstance(text, str) and text, f"{tool_name}: missing tool result")
        body = json.loads(text)
        require(
            isinstance(body, dict) and body.get("success") is not False,
            f"{tool_name}: {body}",
        )
        return body

    def load(self, *names):
        for name in names:
            # load_toolset's first content item is a JSON object.
            self.tool("load_toolset", name=name)


@contextmanager
def server(konnect, kicad, reports, ipc=None):
    config = {"kicad_cli": str(kicad), "transport": "stdio"}
    if ipc is not None:
        config["ipc_address"] = ipc
    path = reports / "config.json"
    write_json(path, config)
    with (
        (reports / "mcp.jsonl").open("x", encoding="utf-8") as trace,
        (reports / "mcp-stderr.log").open("x", encoding="utf-8") as errors,
    ):
        process = subprocess.Popen(
            [konnect, "--config", path],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=errors,
            encoding="utf-8",
        )
        try:
            mcp = Mcp(process, trace)
            initialized = mcp.request(
                "initialize",
                {
                    "protocolVersion": "2025-06-18",
                    "capabilities": {},
                    "clientInfo": {"name": "konnect-routed-kicad-test", "version": "1"},
                },
            )
            require(
                isinstance(initialized.get("serverInfo"), dict),
                "MCP initialize did not identify a server",
            )
            mcp.send(method="notifications/initialized")
            yield mcp
        finally:
            process.stdin.close()
            try:
                process.wait(timeout=3)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=3)
            process.stdout.close()


def build_schematic(mcp, paths, reports):
    schematic = str(paths[1])
    directory = str(paths[0].parent)
    mcp.tool("create_project", path=directory, name=PROJECT)
    require(
        all(path.is_file() for path in paths),
        "MCP create_project omitted a design file",
    )
    for reference, (symbol, footprint, (x, y)) in COMPONENTS.items():
        value = {"value": "10k"} if reference == "R1" else {}
        mcp.tool(
            "add_schematic_component",
            schematic=schematic,
            lib_id=symbol,
            reference=reference,
            footprint=footprint,
            x=x,
            y=y,
            **value,
        )

    pins = {}
    for reference in COMPONENTS:
        observed = mcp.tool(
            "get_schematic_pin_locations", schematic=schematic, reference=reference
        )["pins"]
        pins[reference] = {pin["number"]: (pin["x"], pin["y"]) for pin in observed}
        require(
            len(observed) == 2 and pins[reference].keys() == PIN_NETS.keys(),
            f"unexpected symbol pins for {reference}: {observed}",
        )
    wires, labels = [], {}
    for pin, net in PIN_NETS.items():
        rx, ry = pins["R1"][pin]
        jx, jy = pins["J1"][pin]
        y_stub = round(ry + (-1.27 if pin == "1" else 1.27), 6)
        bend = 114.3, y_stub
        points = [(rx, ry), (rx, y_stub), bend, (bend[0], jy), (jx, jy)]
        wires.extend(
            {"x1": x1, "y1": y1, "x2": x2, "y2": y2}
            for (x1, y1), (x2, y2) in pairwise(points)
            if (x1, y1) != (x2, y2)
        )
        labels[net] = bend
    mcp.tool("batch_add_wire", schematic=schematic, wires=wires)
    for net, (x, y) in labels.items():
        mcp.tool("add_schematic_net_label", schematic=schematic, net=net, x=x, y=y)
    erc = mcp.tool(
        "run_erc",
        schematic=schematic,
        severity="warning",
        output=str(reports / "erc.json"),
    )
    require(
        all(
            type(erc.get(key)) is int and erc[key] == 0
            for key in ("total", "errors", "warnings")
        )
        and erc.get("violations") == [],
        f"ERC is incomplete or has violations: {erc}",
    )
    netlist = reports / "schematic.xml"
    mcp.tool(
        "generate_netlist", schematic=schematic, format="kicadxml", output=str(netlist)
    )
    write_json(
        reports / "pin-membership.json",
        verify_netlist(netlist.read_text(encoding="utf-8")),
    )


def prepare(args, root, reports):
    konnect, kicad = args.konnect.resolve(), args.kicad_cli.resolve()
    receipt = {
        "time": now(),
        "platform": platform.platform(),
        "expected_sha": args.expect_sha,
        "konnect": str(konnect),
        "kicad_cli": str(kicad),
        "executable_sha256": {
            "konnect": hashes([konnect])[konnect.name],
            "kicad_cli": hashes([kicad])[kicad.name],
        },
    }
    write_json(root / "environment.json", receipt)
    with server(konnect, kicad, reports) as mcp:
        info = mcp.tool("get_installation_info")
        write_json(reports / "installation.json", info)
        require(
            info["build"]["commit"] == args.expect_sha,
            "running Konnect build commit differs from --expect-sha",
        )
        require(
            info["kicad"]["probe_status"] == "ok"
            and Path(info["kicad"]["cli_path"]).resolve() == kicad
            and re.match(r"^10\.\d+\.\d+\b", info["kicad"]["version"]),
            f"expected the configured KiCad 10 CLI: {info['kicad']}",
        )
        receipt["kicad_version"] = info["kicad"]["version"]
        write_json(root / "environment.json", receipt)
        mcp.load("project", "sch_components", "sch_wiring", "sch_export")
        for case in CASES:
            path = reports / case
            path.mkdir()
            paths = design(root, case)
            build_schematic(mcp, paths, path)
            write_json(path / "design-sha256.json", hashes(paths))
            print(f"{case} PCB: {paths[0]}", flush=True)


def readiness(mcp, board, timeout):
    deadline = time.monotonic() + timeout
    while (left := deadline - time.monotonic()) > 0:
        response = mcp.tool("open_project", path=board, timeout=left)
        if target_ready(response, Path(board)):
            return
        time.sleep(min(0.25, max(0, deadline - time.monotonic())))
    raise TimeoutError(f"PCB Editor did not identify {board} in {timeout}s")


def get_pads(mcp, board):
    pads = {}
    for reference in COMPONENTS:
        reply = mcp.tool("get_component_pads", board=board, reference=reference)
        require(
            reply.get("source") == "ipc" and reply.get("pad_count") == 2,
            f"expected two live pads for {reference}: {reply}",
        )
        observed = {pad["number"]: pad for pad in reply["pads"]}
        require(
            len(reply["pads"]) == 2 and observed.keys() == PIN_NETS.keys(),
            f"unexpected pad numbering for {reference}: {reply}",
        )
        pads[reference] = observed
    return pads


def synchronize(mcp, board, schematic):
    command = {"schematic": schematic, "board": board}
    plan = mcp.tool("update_pcb_from_schematic", **command, dry_run=True)
    require(
        plan["status"] == "ready"
        and not plan["unassigned_footprints"]
        and plan["coverage"]["footprints_added"]["planned"] == 2
        and plan["coverage"]["skipped_by_flag"]["planned"] == 0
        and plan["coverage"]["conflicts"]["planned"] == 0
        and sorted(
            change["reference"] for change in plan["changes"] if change["kind"] == "add"
        )
        == sorted(COMPONENTS),
        f"unexpected PCB synchronization plan: {plan}",
    )
    applied = mcp.tool(
        "update_pcb_from_schematic",
        **command,
        dry_run=False,
        expected_plan_revision=plan["plan_revision"],
    )
    require(
        applied["status"] == "applied"
        and applied["coverage"]["footprints_added"]["applied"] == 2,
        f"PCB synchronization did not add both parts: {applied}",
    )
    noop = mcp.tool("update_pcb_from_schematic", **command, dry_run=True)
    require(noop["status"] == "noop", f"PCB synchronization did not converge: {noop}")


def layout(mcp, board, *, unrouted):
    (x1, y1), (x2, y2) = OUTLINE[0], OUTLINE[2]
    outline = mcp.tool("add_board_outline", board=board, x1=x1, y1=y1, x2=x2, y2=y2)
    require(
        outline.get("source") == "ipc",
        f"outline was not drawn over live IPC: {outline}",
    )
    mcp.tool(
        "set_component_placements",
        board=board,
        placements=[
            {"reference": reference, "x": x, "y": y, "rotation": rotation}
            for reference, (x, y), rotation in [
                ("R1", ROUTES["LOAD_A"][0], 0),
                ("J1", ROUTES["LOAD_A"][-1], 90),
            ]
        ],
    )
    pads = get_pads(mcp, board)
    layer_list = mcp.tool("get_layer_list", board=board, board_source="live")["layers"]
    require(
        {layer["name"] for layer in layer_list if layer["copper"]} == {"F.Cu", "B.Cu"},
        f"unexpected copper layers: {layer_list}",
    )
    score = mcp.tool("score_placement", board=board)
    require(
        score.get("source") == "ipc"
        and score.get("outline_shape") == "rectangular"
        and score.get("hard_failures") == []
        and score.get("footprints_scored") == 2,
        f"placement was not verified live: {score}",
    )

    nets = {net["name"] for net in mcp.tool("get_nets_list", board=board)["nets"]} - {
        ""
    }
    expected, full_names = [], set()
    omitted = None
    for pin, net in PIN_NETS.items():
        path = ROUTES[net]
        resistor, connector = pads["R1"][pin], pads["J1"][pin]
        full_name = resistor["net"]
        require(
            full_name.removeprefix("/") == net
            and full_name == connector["net"]
            and (resistor["x"], resistor["y"]) == path[0]
            and (connector["x"], connector["y"]) == path[-1],
            f"unexpected live pads for {net}: {pads}",
        )
        full_names.add(full_name)
        if unrouted and net == "LOAD_B":
            omitted = {resistor["uuid"]: "R1", connector["uuid"]: "J1"}
            require(
                len(omitted) == 2 and all(omitted),
                "missing live pad identity for LOAD_B",
            )
            continue
        for (x1, y1), (x2, y2) in pairwise(path):
            mcp.tool(
                "route_trace",
                board=board,
                net_name=full_name,
                layer="F.Cu",
                width=0.3,
                x1=x1,
                y1=y1,
                x2=x2,
                y2=y2,
            )
        expected.extend((full_name, edge) for edge in segments(path))
    require(nets == full_names, f"unexpected live board nets: {nets}")
    traces = mcp.tool("query_traces", board=board)
    observed, seen = [], set()
    for trace in traces["traces"]:
        require(
            trace["layer"] == "F.Cu"
            and trace["width"] == 0.3
            and trace["uuid"]
            and trace["uuid"] not in seen,
            f"unexpected live trace: {trace}",
        )
        seen.add(trace["uuid"])
        observed.append(
            (
                trace["net"],
                canonical_segment(
                    (trace["x1"], trace["y1"]),
                    (trace["x2"], trace["y2"]),
                ),
            )
        )
    require(
        traces["count"] == len(expected) and sorted(observed) == sorted(expected),
        f"live board has the wrong tracks: {traces}",
    )
    return omitted


def snapshot_drc(mcp, board, reports, name):
    """Save the exact live target, verify its snapshot and retain the DRC report."""
    path = reports / f"{name}.json"
    summary = mcp.tool(
        "run_drc",
        board=board,
        sync_live_board=True,
        refill_zones=False,
        severity="info",
        output=str(path),
    )
    write_json(reports / f"{name}-summary.json", summary)
    verify_drc_source(summary)
    return summary, read_json(path)


def run(args, root, reports):
    require(
        read_json(root / "prepare" / "result.json")["status"] == "pass",
        "project preparation did not pass",
    )
    require(
        args.ipc.startswith("ipc://"), "use the exact PCB Editor IPC socket as ipc://…"
    )
    env = read_json(root / "environment.json")
    konnect, kicad = Path(env["konnect"]), Path(env["kicad_cli"])
    for key, binary in (("konnect", konnect), ("kicad_cli", kicad)):
        require(
            hashes([binary])[binary.name] == env["executable_sha256"][key],
            f"{key} executable changed after preparation",
        )
    paths = design(root, args.case)
    board, schematic = map(str, paths[:2])
    require(
        paths[0].parent.resolve() == root / args.case,
        "case directory must stay inside the test workspace",
    )
    require(
        hashes(paths) == read_json(root / "prepare" / args.case / "design-sha256.json"),
        "prepared project changed before first save",
    )
    output = root / args.case / "gerbers"
    require(
        not output.exists() and not output.is_symlink(),
        "fabrication output already exists",
    )

    with server(konnect, kicad, reports, args.ipc) as mcp:
        mcp.load(
            "project",
            "editor_navigation",
            "sch_export",
            "pcb_board",
            "pcb_components",
            "pcb_routing",
            "placement",
            "verification",
            "pcb_export",
        )
        readiness(mcp, board, args.ready_timeout)
        editor = mcp.tool("get_editor_state")
        version = editor.get("kicad_version")
        require(
            version
            and f"{version['major']}.{version['minor']}.{version['patch']}"
            == env["kicad_version"].split()[0],
            "KiCad CLI and PCB Editor versions differ",
        )
        write_json(reports / "editor-state.json", editor)
        # Bootstrap defaults through a bound PCB save. The intentionally empty
        # board's preliminary DRC is evidence of synchronization, not the gate.
        snapshot_drc(mcp, board, reports, "bootstrap-drc")
        require(
            hashes([paths[1]])[paths[1].name]
            == read_json(root / "prepare" / args.case / "design-sha256.json")[
                paths[1].name
            ],
            "first PCB save changed the evaluated schematic",
        )
        synchronize(mcp, board, schematic)
        unrouted = args.case == "unrouted"
        omitted = layout(mcp, board, unrouted=unrouted)
        require(
            (omitted is not None) == unrouted,
            "layout did not establish the expected control",
        )

        snapshot_drc(mcp, board, reports, "saved-snapshot-drc")
        inputs = hashes(paths)
        write_json(reports / "input-sha256.json", inputs)
        summary, report = snapshot_drc(mcp, board, reports, "drc")
        require(hashes(paths) == inputs, "design inputs changed during DRC")
        approved = verify_drc(summary, report, omitted_pads=omitted)
        if unrouted:
            require(
                not approved
                and "export_gerber" not in mcp.called_tools
                and not output.exists()
                and not output.is_symlink(),
                "the unrouted control attempted manufacturing output",
            )
            write_json(
                reports / "fabrication.json",
                {"export_invoked": False, "output_directory_exists": False},
            )
            return

        require(
            approved and hashes(paths) == inputs,
            "manufacturing input lost DRC approval",
        )
        require(
            not output.exists() and not output.is_symlink(),
            "fabrication output already exists",
        )
        exported = mcp.tool(
            "export_gerber",
            board=board,
            output_dir=str(output),
            layers=list(LAYERS),
            drill_file=True,
        )
        require(hashes(paths) == inputs, "design inputs changed during export")
        evidence = verify_fabrication(output, exported["files"])
        write_json(
            reports / "fabrication.json",
            dict(
                evidence, export_invoked=True, sha256=hashes(sorted(output.iterdir()))
            ),
        )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    setup = sub.add_parser(
        "prepare",
        help="create two fresh disposable projects and check their schematics",
    )
    setup.add_argument("--work-dir", type=Path, required=True)
    setup.add_argument("--konnect", type=Path, required=True)
    setup.add_argument("--kicad-cli", type=Path, required=True)
    setup.add_argument("--expect-sha", required=True)
    execute = sub.add_parser(
        "run", help="verify one prepared case in its own PCB Editor session"
    )
    execute.add_argument("--work-dir", type=Path, required=True)
    execute.add_argument("--case", choices=CASES, required=True)
    execute.add_argument("--ipc", required=True)
    execute.add_argument("--ready-timeout", type=int, default=25)
    args = parser.parse_args()
    root = args.work_dir.resolve()
    if args.command == "prepare":
        if not re.fullmatch(r"[0-9a-f]{40}", args.expect_sha):
            parser.error(
                "--expect-sha must be the full commit SHA used to build Konnect"
            )
        root.mkdir()
        reports = root / "prepare"
        action = prepare
    else:
        if not 1 <= args.ready_timeout <= 120:
            parser.error("--ready-timeout must be within 1–120 seconds")
        reports = root / f"run-{args.case}"
        action = run
    # One attempt per case. Refuse replay after failures or uncertain mutations.
    reports.mkdir()
    result = {
        "command": args.command,
        "case": getattr(args, "case", None),
        "started": now(),
        "status": "running",
    }
    path = reports / "result.json"
    write_json(path, result)
    try:
        action(args, root, reports)
    except (Exception, KeyboardInterrupt) as error:
        write_json(
            path,
            dict(
                result,
                status="fail",
                finished=now(),
                detail=f"{type(error).__name__}: {error}",
            ),
        )
        raise
    write_json(path, dict(result, status="pass", finished=now()))
    print(f"PASS {args.command} {getattr(args, 'case', '')}: {reports}")


if __name__ == "__main__":
    main()
