import contextlib
import hashlib
import io
import json
from pathlib import Path
import tarfile
import tempfile
import unittest
from argparse import Namespace
from unittest.mock import Mock, patch

import import_plutus
import upstreams
from conformance import ROOT


class SourceTests(unittest.TestCase):
    def test_cached_marker_cannot_hide_changed_or_added_sources(self):
        with tempfile.TemporaryDirectory() as temp, patch.object(upstreams, "ROOT", Path(temp)):
            cache = Path(temp) / ".cache/upstreams"
            cache.mkdir(parents=True)
            archive = cache / "test-rev.tar.gz"
            with tarfile.open(archive, "w:gz") as tar:
                member = tarfile.TarInfo("test-rev/source.rs")
                member.size = 8
                tar.addfile(member, io.BytesIO(b"original"))
            pin = dict(repository="test/test", revision="rev", archive_sha256=hashlib.sha256(archive.read_bytes()).hexdigest())
            with contextlib.redirect_stdout(io.StringIO()):
                upstreams.fetch("test", pin)
            checkout = upstreams.verify_source("test", pin)
            (checkout / "source.rs").write_text("modified")
            with self.assertRaisesRegex(ValueError, "differs"):
                upstreams.verify_source("test", pin)
            (checkout / "source.rs").write_text("original")
            (checkout / "extra.rs").write_text("extra")
            with self.assertRaisesRegex(ValueError, "unexpected"):
                upstreams.verify_source("test", pin)


class ImportTests(unittest.TestCase):
    def setup_corpus(self, root):
        pins = json.loads((ROOT / "upstreams.lock.json").read_text())
        (root / "upstreams.lock.json").write_text(json.dumps(pins))
        (root / "profiles").mkdir()
        (root / "profiles/plutus-v3-pv11.json").write_bytes((ROOT / "profiles/plutus-v3-pv11.json").read_bytes())
        checkout = root / "checkout"
        corpus = checkout / "plutus-conformance/test-cases/uplc/evaluation"
        corpus.mkdir(parents=True)
        for name in ("a", "b"):
            source = corpus / (name + ".uplc")
            source.write_text("(program 1.0.0 (con integer 42))\n")
            source.with_suffix(".uplc.expected").write_bytes(b"(program 1.0.0 (con integer 42))\r\n")
            source.with_suffix(".uplc.budget.expected").write_text("({cpu: 16100\n| mem: 200})\n")
        return checkout

    def arguments(self, root):
        return Namespace(normalizer=["first", "second"], timeout=1, limit=None, profile="profiles/plutus-v3-pv11.json", output=root / "output.jsonl")

    def normalizers(self, factory, outcomes):
        engines = []
        for i, outcome in enumerate(outcomes):
            engine = Mock()
            engine.evaluate.return_value = {"engine": f"normalizer-{i}", "revision": "pin", "outcome": outcome}
            engines.append(engine)
        factory.side_effect = engines
        return engines

    def test_failed_import_does_not_publish_a_partial_corpus(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            checkout = self.setup_corpus(root)
            args = self.arguments(root)
            with patch.object(import_plutus, "ROOT", root), patch.object(import_plutus, "verify_source", return_value=checkout), patch.object(import_plutus, "Engine") as engine:
                good = {"status": "success", "term": ["constant", ["integer", "42"]]}
                first, _ = self.normalizers(engine, [good, good])
                response = first.evaluate.return_value
                first.evaluate.side_effect = [response, dict(response, outcome={"status": "infrastructure_error", "diagnostic": "crash"})]
                with self.assertRaises(RuntimeError):
                    import_plutus.main(args)
                self.assertFalse(args.output.exists())
                self.assertFalse(list(root.glob("tmp*")))

    def test_raw_goldens_survive_unavailable_normalization(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            checkout = self.setup_corpus(root)
            args = self.arguments(root)
            with patch.object(import_plutus, "ROOT", root), patch.object(import_plutus, "verify_source", return_value=checkout), patch.object(import_plutus, "Engine") as engine:
                self.normalizers(engine, [{"status": "unsupported", "reason": "unhandled constant"}] * 2)
                with contextlib.redirect_stdout(io.StringIO()):
                    import_plutus.main(args)
                cases = [json.loads(line) for line in args.output.read_text().splitlines()]
                self.assertEqual(len(cases), 2)
                for case in cases:
                    self.assertIn("unavailable", case)
                    self.assertEqual(case["expected"]["budget"], {"cpu": "16100", "mem": "200"})
                    for golden in case["provenance"]["goldens"].values():
                        self.assertEqual(golden["text"], (checkout / golden["path"]).read_bytes().decode("utf-8"))
                        self.assertEqual(golden["sha256"], hashlib.sha256(golden["text"].encode()).hexdigest())
                before = args.output.read_bytes()
                self.normalizers(engine, [{"status": "unsupported", "reason": "unhandled constant"}] * 2)
                with self.assertRaisesRegex(ValueError, "overwrite"):
                    import_plutus.main(args)
                self.assertEqual(args.output.read_bytes(), before)

    def test_parser_disagreement_cannot_become_a_golden(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            checkout = self.setup_corpus(root)
            args = self.arguments(root)
            with patch.object(import_plutus, "ROOT", root), patch.object(import_plutus, "verify_source", return_value=checkout), patch.object(import_plutus, "Engine") as engine:
                self.normalizers(engine, [{"status": "success", "term": ["constant", ["string", value]]}
                                          for value in (r"\172", "¬")])
                with contextlib.redirect_stdout(io.StringIO()):
                    import_plutus.main(args)
                for line in args.output.read_text().splitlines():
                    case = json.loads(line)
                    self.assertIn("unavailable", case)
                    self.assertNotIn("term", case["expected"])


if __name__ == "__main__":
    unittest.main()
