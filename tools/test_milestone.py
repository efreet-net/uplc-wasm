"""The scoped Flat corpus must retain independent results/costs and provenance."""
import copy
import hashlib
import json
from pathlib import Path
import shutil
import tempfile
import unittest

import build_milestone_corpus as milestone
from conformance import ROOT, load_cases


class MilestoneTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.pins = json.loads((ROOT / "upstreams.lock.json").read_text())
        cls.cases = load_cases([ROOT / milestone.CORPUS])

    def copy_fixture_tree(self, root):
        for file in ("upstreams.lock.json", milestone.PROFILE, milestone.CORPUS,
                     *milestone.EXTRA_CORPORA.values()):
            target = root / file
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(ROOT / file, target)
        shutil.copytree(ROOT / "fixtures/milestone", root / "fixtures/milestone")

    def mutate_case(self, mutation, match):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            self.copy_fixture_tree(root)
            cases = copy.deepcopy(self.cases)
            mutation(cases)
            (root / milestone.CORPUS).write_bytes(milestone.encode_jsonl(cases))
            with self.assertRaisesRegex(ValueError, match):
                milestone.verify_committed(root)

    def test_committed_provenance_is_complete_and_strict_corpus_has_goldens(self):
        self.assertEqual(milestone.verify_committed(),
                         {"milestone": 68, "decoder": 18, "unsupported": 7, "audit": 3})
        self.assertTrue(all(case["program"]["format"] == "flat" for case in self.cases))
        self.assertTrue(all("expected" in case and "unavailable" not in case for case in self.cases))
        self.assertEqual(len([case for case in self.cases if case["expected"]["status"] == "success"]), 48)

    def test_changed_flat_even_with_recomputed_hash_is_rejected(self):
        def mutate(cases):
            case = cases[0]
            case["program"]["hex"] += "00"
            case["provenance"]["flat_sha256"] = hashlib.sha256(bytes.fromhex(case["program"]["hex"])).hexdigest()
        self.mutate_case(mutate, "encoders disagree")

    def test_raw_official_source_or_result_cannot_be_rewritten(self):
        for field in ("source", "result", "budget"):
            def mutate(cases):
                p = cases[0]["provenance"]
                record = p["source"] if field == "source" else p["goldens"][field]
                record["text"] += "\n"
                record["sha256"] = hashlib.sha256(record["text"].encode()).hexdigest()
            with self.subTest(field=field):
                self.mutate_case(mutate, "raw provenance mismatch")

    def test_official_and_manually_derived_costs_cannot_be_changed(self):
        for kind in ("official-plutus-derived-flat", "independent-cek-derivation"):
            def mutate(cases):
                case = next(case for case in cases if case["provenance"]["kind"] == kind and case["expected"]["status"] == "success")
                case["expected"]["budget"]["cpu"] = "0"
            with self.subTest(kind=kind):
                self.mutate_case(mutate, "expected result or budget")

    def test_normalization_disagreement_cannot_become_an_expected_term(self):
        def mutate(cases):
            cases[0]["provenance"]["normalizers"][1]["outcome"]["term"] = ["constant", ["integer", "2"]]
        self.mutate_case(mutate, "normalizer disagreement")

    def test_candidate_or_duplicate_reference_cannot_supply_golden(self):
        for engine in ("uplc-core", "aiken-normalizer"):
            def mutate(cases):
                cases[0]["provenance"]["normalizers"][1]["engine"] = engine
            with self.subTest(engine=engine):
                self.mutate_case(mutate, "both pinned normalizer")

    def test_evaluation_response_is_not_a_golden_parser_response(self):
        records = copy.deepcopy(self.cases[0]["provenance"]["normalizers"])
        records[0]["outcome"]["budget"] = {"cpu": "16100", "mem": "200"}
        with self.assertRaisesRegex(ValueError, "without evaluation"):
            milestone.reference_records(records, "normalizer", self.pins)

    def test_boundary_cannot_be_relaxed_to_hide_budget_exhaustion(self):
        def mutate(cases):
            case = next(case for case in cases if case["id"].endswith("/budget-cpu-minus-one"))
            case["mode"]["budget"]["cpu"] = case["provenance"]["budget_boundary"]["required_success_budget"]["cpu"]
        self.mutate_case(mutate, "budget boundary limit")

    def test_success_golden_and_derivation_cannot_be_replaced(self):
        def mutate(cases):
            case = next(case for case in cases if case["id"] == "milestone/probe/captured-function-binders")
            case["expected"]["term"] = ["lambda", ["var", "1"]]
        self.mutate_case(mutate, "expected result or budget")

    def test_unsupported_and_reference_disagreements_remain_visible(self):
        unsupported = load_cases([ROOT / milestone.UNSUPPORTED_CORPUS])
        self.assertTrue(all("expected" not in case for case in unsupported))
        audit = load_cases([ROOT / milestone.AUDIT_CORPUS])
        self.assertEqual({case["id"].rsplit("/", 1)[1] for case in audit},
                         {"force-error-startup-only", "zero-debruijn-reference-policy", "unbound-debruijn-reference-panic"})
        for case in audit:
            self.assertNotEqual(case.get("expected", {}).get("status"), "unsupported")

    def test_failed_publication_preserves_existing_corpus(self):
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / "corpus.jsonl"
            path.write_bytes(b"original\n")
            with self.assertRaises(FileExistsError):
                milestone.publish(path, b"replacement\n")
            self.assertEqual(path.read_bytes(), b"original\n")
            self.assertEqual(list(Path(temp).iterdir()), [path])


if __name__ == "__main__":
    unittest.main()
