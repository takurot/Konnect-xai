"""Fast controls for the opt-in runner's acceptance checks (no KiCad needed)."""

import json
import unittest
from pathlib import Path
from tempfile import TemporaryDirectory
from unittest.mock import Mock

from evidence import (
    LAYERS,
    OUTLINE,
    ROUTES,
    target_ready,
    verify_drc,
    verify_fabrication,
    verify_netlist,
)
from run import snapshot_drc


class DrcTests(unittest.TestCase):
    def report(self, omitted=False):
        rules = []
        if omitted:
            rules = [
                {
                    "severity": "error",
                    "rule": "unconnected_items",
                    "items": [
                        {
                            "uuid": uuid,
                            "item_kind": "pad",
                            "ownership_status": "resolved",
                            "owner": {"kind": "footprint", "reference": reference},
                        }
                        for uuid, reference in (("pad-r", "R1"), ("pad-j", "J1"))
                    ],
                }
            ]
        summary = {
            "source": "saved_file",
            "live_board_synced": True,
            "categories_not_reported": [],
            "severity_filter": "info",
            "truncated": False,
            "unconnected_items": len(rules),
            "design_rule_violations": 0,
            "schematic_parity": 0,
            "total_violations": len(rules),
            "errors": len(rules),
            "warnings": 0,
            "violations": rules,
            "filtered_count": len(rules),
            "shown": len(rules),
        }
        return summary, {
            "violations": [],
            "unconnected_items": rules,
            "schematic_parity": [],
        }

    def test_snapshot_requests_targeted_save_and_requires_sync_evidence(self):
        summary, report = self.report(omitted=True)
        with TemporaryDirectory() as folder:
            reports = Path(folder)
            board = str(reports / "exact-target.kicad_pcb")
            path = reports / "bootstrap.json"
            path.write_text(json.dumps(report), encoding="utf-8")
            mcp = Mock()
            mcp.tool.return_value = summary
            self.assertEqual(
                snapshot_drc(mcp, board, reports, "bootstrap"), (summary, report)
            )
            mcp.tool.assert_called_once_with(
                "run_drc",
                board=board,
                sync_live_board=True,
                refill_zones=False,
                severity="info",
                output=str(path),
            )
            self.assertEqual(
                json.loads((reports / "bootstrap-summary.json").read_text()), summary
            )
            summary["live_board_synced"] = False
            with self.assertRaisesRegex(ValueError, "saved live board"):
                snapshot_drc(mcp, board, reports, "bootstrap")

    def test_clean_drc_allows_export_and_unrouted_control_does_not(self):
        self.assertTrue(verify_drc(*self.report(), omitted_pads=None))
        self.assertFalse(
            verify_drc(
                *self.report(omitted=True), omitted_pads={"pad-r": "R1", "pad-j": "J1"}
            )
        )

    def test_null_missing_non_integer_and_unreported_counts_fail(self):
        for key, value in (
            ("schematic_parity", None),
            ("errors", False),
            ("categories_not_reported", ["schematic_parity"]),
            ("warnings", 1),
        ):
            with self.subTest(key=key):
                summary, report = self.report()
                summary[key] = value
                with self.assertRaisesRegex(ValueError, key):
                    verify_drc(summary, report, omitted_pads=None)
        summary, report = self.report()
        del report["unconnected_items"]
        with self.assertRaisesRegex(ValueError, "unconnected_items"):
            verify_drc(summary, report, omitted_pads=None)

    def test_wrong_unconnected_rule_or_pad_fails(self):
        for field, value in (("rule", "clearance"), ("items", [{"uuid": "wrong"}])):
            summary, report = self.report(omitted=True)
            report["unconnected_items"][0][field] = value
            with self.assertRaisesRegex(ValueError, "unconnected"):
                verify_drc(summary, report, omitted_pads={"pad-r": "R1", "pad-j": "J1"})


class TargetTests(unittest.TestCase):
    def test_waits_for_target_and_refuses_other_open_boards(self):
        target = Path.cwd() / "disposable.kicad_pcb"
        response = {
            "requested_board": str(target),
            "requested_open": False,
            "open_boards": [],
        }
        self.assertFalse(target_ready(response, target))
        response.update(requested_open=True, open_boards=[str(target)])
        self.assertTrue(target_ready(response, target))
        response["open_boards"].append(str(target.with_name("foreign.kicad_pcb")))
        with self.assertRaisesRegex(ValueError, "open board"):
            target_ready(response, target)


class NetlistTests(unittest.TestCase):
    def test_exact_net_member_pairs_and_no_duplicates(self):
        xml = """<export><nets>
          <net name="/LOAD_A"><node ref="J1" pin="1"/><node ref="R1" pin="1"/></net>
          <net name="/LOAD_B"><node ref="J1" pin="2"/><node ref="R1" pin="2"/></net>
        </nets></export>"""
        self.assertEqual(verify_netlist(xml)["LOAD_B"], [("J1", "2"), ("R1", "2")])
        with self.assertRaisesRegex(ValueError, "netlist"):
            verify_netlist(xml.replace('ref="R1" pin="2"', 'ref="J1" pin="2"'))


class FabricationTests(unittest.TestCase):
    HOLES = ((112, 108), (114.54, 116), (119.62, 108), (117.08, 116))

    @staticmethod
    def coordinate(point, op):
        return f"X{round(point[0] * 1e6)}Y{round(-point[1] * 1e6)}D0{op}*\n"

    def setUp(self):
        directory = TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.directory = Path(directory.name)
        attributes = []
        for layer, function in LAYERS.items():
            name = f"{layer}.plot"
            attributes.append(
                {"Path": name, "FileFunction": function.removesuffix(",NP")}
            )
            body = f"%TF.FileFunction,{function}*%\n%FSLAX46Y46*%\n%MOMM*%\n"
            body += "G01*\n%ADD10C,0.300000*%\nD10*\n"
            if layer in ("F.Cu", "B.Cu", "F.Mask", "B.Mask"):
                body += "".join(self.coordinate(point, 3) for point in self.HOLES)
            if layer == "F.Cu":
                for net, points in ROUTES.items():
                    body += f"%TO.N,/{net}*%\n" + self.polyline(points)
            if layer == "Edge.Cuts":
                body += self.polyline(OUTLINE)
            (self.directory / name).write_text(body + "M02*\n", encoding="utf-8")
        self.job = self.directory / "board.gbrjob"
        self.job.write_text(
            json.dumps({"FilesAttributes": attributes}), encoding="utf-8"
        )
        header = "M48\n; FORMAT={-:-/ absolute / metric / decimal}\n"
        self.drill = self.directory / "board-PTH.drl"
        self.drill.write_text(
            header + "; #@! TF.FileFunction,Plated,1,2,PTH\nFMAT,2\nMETRIC\n"
            "T1C0.8\n%\nG90\nG05\nT1\n"
            + "".join(f"X{x}Y{-y}\n" for x, y in self.HOLES)
            + "M30\n",
            encoding="utf-8",
        )

    def polyline(self, points):
        return self.coordinate(points[0], 2) + "".join(
            self.coordinate(point, 1) for point in points[1:]
        )

    def verify(self):
        return verify_fabrication(
            self.directory, [p.name for p in self.directory.iterdir()]
        )

    def test_layer_names_come_from_file_function_and_manifest(self):
        result = self.verify()
        self.assertEqual(len(result["layers"]), 7)
        self.assertEqual(result["pth_holes"], sorted(self.HOLES))

    def test_gerber_mask_accepts_kicad_aperture_macro_without_hiding_geometry(self):
        mask = self.directory / "F.Mask.plot"
        text = mask.read_text(encoding="utf-8")
        macro = "%AMRoundRect*\n0 Comment*\n1,1,$1+$1,$2,$3*%\n"
        mask.write_text(
            text.replace("%ADD10C,0.300000*%", macro + "%ADD10RoundRect,0.3*%"),
            encoding="utf-8",
        )
        self.verify()
        mask.write_text(text.replace("D10*", "D20*"), encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "undefined aperture"):
            self.verify()

    def test_malformed_front_and_back_silk_bodies_fail(self):
        for layer in ("F.SilkS", "B.SilkS"):
            with self.subTest(layer=layer):
                path = self.directory / f"{layer}.plot"
                body = path.read_text(encoding="utf-8")
                path.write_text(
                    body.replace("M02*", "NOT-GERBER*\nM02*"), encoding="utf-8"
                )
                with self.assertRaisesRegex(ValueError, layer):
                    self.verify()
                path.write_text(body, encoding="utf-8")

    def test_aperture_needs_supported_shape_dimensions_or_declared_macro(self):
        path = self.directory / "F.Cu.plot"
        source = path.read_text(encoding="utf-8")
        for definition in (
            "Bogus,garbage",
            "RoundRect,0.3",
            "C,-0.2",
            "R,0.2",
            "O,1X0",
        ):
            with self.subTest(definition=definition):
                path.write_text(
                    source.replace("C,0.300000", definition), encoding="utf-8"
                )
                with self.assertRaisesRegex(ValueError, "F.Cu.*aperture"):
                    self.verify()

    def test_copper_extra_segment_does_not_pass_on_back(self):
        path = self.directory / "B.Cu.plot"
        body = path.read_text(encoding="utf-8")
        path.write_text(
            body.replace("M02*", self.polyline(ROUTES["LOAD_A"]) + "M02*"),
            encoding="utf-8",
        )
        with self.assertRaisesRegex(ValueError, "B.Cu"):
            self.verify()

    def test_open_profile_and_wrong_mask_coordinates_are_detected(self):
        for layer, operation in (("Edge.Cuts", 1), ("F.Mask", 3)):
            with self.subTest(layer=layer):
                path = self.directory / f"{layer}.plot"
                body = path.read_text(encoding="utf-8")
                lines = body.splitlines(keepends=True)
                index = next(
                    i
                    for i, line in enumerate(lines)
                    if line.endswith(f"D0{operation}*\n")
                )
                path.write_text(
                    "".join(lines[:index] + lines[index + 1 :]), encoding="utf-8"
                )
                with self.assertRaisesRegex(ValueError, layer):
                    self.verify()
                path.write_text(body, encoding="utf-8")

    def test_extra_or_unsupported_drill_operation_fails(self):
        body = self.drill.read_text(encoding="utf-8")
        for extra in ("X113.0Y-108.0\n", "G85X113.0Y-108.0\n"):
            self.drill.write_text(body.replace("M30", extra + "M30"), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "drill"):
                self.verify()

    def test_manifest_path_cannot_escape_fresh_output(self):
        body = json.loads(self.job.read_text(encoding="utf-8"))
        body["FilesAttributes"][0]["Path"] = "../foreign.plot"
        self.job.write_text(json.dumps(body), encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "Gerber manifest"):
            self.verify()


if __name__ == "__main__":
    unittest.main()
