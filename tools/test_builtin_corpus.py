"""Independent builtin fixture derivations, parser boundaries, and preservation."""
import copy
import json
from pathlib import Path
import shutil
import tempfile
import unittest
from unittest.mock import Mock

import build_builtin_corpus as builtin
from build_milestone_corpus import attach_flat, encode_jsonl, publish, reference_records, sha256
from conformance import ROOT, load_cases
from generate_cases import execution_ledger, generate


class CostDerivationTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.parameters = json.loads((ROOT / builtin.PROFILE).read_text())["cost_model"]["parameters"]

    def test_integer_memory_uses_magnitude_64_bit_units(self):
        for number, expected in ((0, 1), (1, 1), (2**63, 1), (2**64-1, 1),
                                 (2**64, 2), (2**128-1, 2), (2**128, 3)):
            for sign in (1, -1):
                self.assertEqual(builtin.integer_memory(str(sign * number)), expected)
        # Longer than Python's default int-from-decimal security bound, without
        # changing that process-global bound or consulting BigInt limb widths.
        self.assertEqual(builtin.integer_memory("1" + "0" * 10000), 520)

    def test_actual_profile_costs_are_independent_known_values(self):
        expected = {"addInteger": (102048, 4), "subtractInteger": (102048, 4),
                    "multiplyInteger": (93548, 5), "equalsInteger": (52891, 1),
                    "lessThanInteger": (45831, 1), "lessThanEqualsInteger": (44389, 1),
                    "ifThenElse": (76049, 1)}
        for name, (cpu, mem) in expected.items():
            self.assertEqual(builtin.builtin_budget(name, [str(2**64), str(2**128)], self.parameters),
                             {"cpu": str(cpu), "mem": str(mem)})

    def test_type_error_ledger_omits_builtin_cost_and_pending_steps(self):
        events = ["apply", "apply", "builtin", "constant", "constant", "error"]
        self.assertEqual(builtin.ledger_outcome(events, self.parameters, builtin.LIMIT),
                         {"status": "failure", "kind": "evaluation", "budget": {"cpu": "100", "mem": "100"}, "traces": []})

    def test_builtin_charge_is_immediate_before_pending_batch(self):
        events = ["apply", "apply", "builtin", "constant", "constant",
                  {"builtin": "addInteger", "arguments": ["1", "2"]}, "error"]
        self.assertEqual(builtin.ledger_outcome(events, self.parameters, builtin.LIMIT)["budget"],
                         {"cpu": "101308", "mem": "102"})
        limit = {"cpu": "101307", "mem": "1000000"}
        actual = builtin.ledger_outcome(events, self.parameters, limit)
        self.assertEqual((actual["kind"], actual["budget"]),
                         ("budget_exhausted", {"cpu": "101308", "mem": "102"}))

    def test_200th_visit_flushes_before_builtin_application_and_by_category(self):
        # 199 constants are pending; builtin is the 200th event. The constant
        # category exhausts first, so the builtin CEK/application never charge.
        events = ["constant"] * 199 + ["builtin", {"builtin": "addInteger", "arguments": ["1", "2"]}, "halt"]
        actual = builtin.ledger_outcome(events, self.parameters, {"cpu": "100", "mem": "100"})
        self.assertEqual(actual["budget"], {"cpu": "3184100", "mem": "20000"})
        self.assertEqual(actual["kind"], "budget_exhausted")

    def test_custom_negative_coefficients_and_checked_overflow(self):
        p = ["0"] * 350
        p[0:4] = ["-10", "11", "-1", "2"]
        self.assertEqual(builtin.builtin_budget("addInteger", ["1", "2"], p), {"cpu": "1", "mem": "1"})
        events = ["builtin", {"builtin": "addInteger", "arguments": ["1", "2"]}, "halt"]
        p[0:4] = [str(2**63-1), "1", "0", "0"]
        self.assertIsNone(builtin.ledger_outcome(events, p, builtin.LIMIT)["budget"])
        p[0:4] = ["-2", "1", "0", "0"]
        with self.assertRaisesRegex(ValueError, "negative computed charge"):
            builtin.ledger_outcome(events, p, builtin.LIMIT)

    def test_seed_arithmetic_and_wrappers_remain_independent_and_reproducible(self):
        first, repeated = list(generate(42, 1000)), list(generate(42, 1000))
        self.assertEqual(first, repeated)
        self.assertNotEqual(first[:10], list(generate(43, 10)))
        for case in first:
            arithmetic = case["provenance"]["arithmetic"]
            a, b = map(int, arithmetic["arguments"])
            value = {"addInteger": a+b, "subtractInteger": a-b, "multiplyInteger": a*b}[arithmetic["builtin"]]
            self.assertEqual(case["expected"]["term"], ["constant", ["integer", str(value)]])
            events = execution_ledger(arithmetic)
            count = 5 + sum(3 if x == "identity" else 2 for x in arithmetic["wrappers_inner_to_outer"])
            self.assertEqual(sum(isinstance(x, str) and x != "halt" for x in events), count)
            result = builtin.ledger_outcome(events, self.parameters, case["mode"]["budget"], case["expected"]["term"])
            application = builtin.builtin_budget(arithmetic["builtin"], arithmetic["arguments"], self.parameters)
            self.assertEqual(result["budget"], {"cpu": str(100+16000*count+int(application["cpu"])),
                                                 "mem": str(100+100*count+int(application["mem"]))})


class ProvenanceTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.cases = load_cases([ROOT / builtin.CORPUS])
        cls.pins = json.loads((ROOT / "upstreams.lock.json").read_text())

    def copy_tree(self, root):
        for path in ("upstreams.lock.json", builtin.PROFILE, builtin.CORPUS,
                     builtin.CANDIDATE_CORPUS, builtin.DECODER_CORPUS,
                     builtin.AUDIT_CORPUS, builtin.UNSUPPORTED_CORPUS,
                     "fixtures/milestone-unsupported.jsonl"):
            destination = root / path
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(ROOT / path, destination)
        shutil.copytree(ROOT / "fixtures/builtins", root / "fixtures/builtins")

    def mutate(self, operation, match, corpus=builtin.CORPUS):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            self.copy_tree(root)
            cases = load_cases([root / corpus])
            operation(cases)
            (root / corpus).write_bytes(encode_jsonl(cases))
            with self.assertRaisesRegex(ValueError, match):
                builtin.verify_committed(root)

    def test_scoped_corpus_is_complete_and_original_regressions_unchanged(self):
        counts = builtin.verify_committed()
        self.assertEqual(counts["official"], 39)
        self.assertEqual(counts["graduated"], 1)
        self.assertGreater(counts["builtins"], 100)
        self.assertGreater(counts["audit"], 10)
        self.assertEqual(counts["decoder"], 8)
        self.assertEqual(counts["unsupported"], 14)
        self.assertTrue(all(case["program"]["format"] == "flat" for case in self.cases))
        self.assertTrue(all("expected" in case and "unavailable" not in case for case in self.cases))
        from build_milestone_corpus import verify_committed as verify_previous
        self.assertEqual(verify_previous(), {"milestone": 68, "decoder": 18, "unsupported": 7, "audit": 3})

    def test_raw_official_bytes_and_costs_are_authoritative(self):
        for field in ("source", "result", "budget"):
            def change(cases):
                p = cases[0]["provenance"]
                record = p["source"] if field == "source" else p["goldens"][field]
                record["text"] += "\n"
                record["sha256"] = sha256(record["text"].encode())
            self.mutate(change, "raw provenance mismatch")
        def cost(cases):
            cases[0]["expected"]["budget"]["cpu"] = "0"
        self.mutate(cost, "expected result or budget")

    def test_rehashed_flat_cannot_override_both_encoders(self):
        def change(cases):
            cases[0]["program"]["hex"] += "00"
            cases[0]["provenance"]["flat_sha256"] = sha256(bytes.fromhex(cases[0]["program"]["hex"]))
        self.mutate(change, "Flat bytes disagree")

    def test_duplicate_or_candidate_parser_identity_rejected(self):
        for identity in ("uplc-core", "aiken-normalizer"):
            def change(cases):
                cases[0]["provenance"]["normalizers"][1]["engine"] = identity
            self.mutate(change, "both pinned normalizer")

    def test_parser_disagreement_cannot_supply_expected_term(self):
        def change(cases):
            cases[0]["provenance"]["normalizers"][1]["outcome"]["term"] = ["error"]
        self.mutate(change, "normalizer disagreement")

    def test_encoder_disagreement_does_not_mutate_case(self):
        case = {"id": "unit/encode", "profile": builtin.PROFILE,
                "program": {"format": "uplc_text", "source": "(program 1.0.0 (builtin addInteger))"},
                "mode": {"kind": "restricting", "budget": builtin.LIMIT}, "provenance": {}}
        before = copy.deepcopy(case)
        engines = []
        for name, value in (("aiken", "0100007001"), ("amaru", "0100007000")):
            engine = Mock()
            engine.evaluate.return_value = {"engine": name + "-flat-encoder", "revision": self.pins[name]["revision"],
                                           "outcome": {"status": "success", "term": ["constant", ["bytes", value]],
                                                       "budget": {"cpu": "0", "mem": "0"}, "traces": []}}
            engines.append(engine)
        with self.assertRaisesRegex(ValueError, "flat-encoder disagreement"):
            attach_flat(case, self.pins, engines)
        self.assertEqual(case, before)

    def test_graduation_retains_original_record_and_bytes(self):
        def change(cases):
            case = next(case for case in cases if "graduation" in case["provenance"])
            case["provenance"]["graduation"]["original_provenance"]["kind"] = "rewritten"
        self.mutate(change, "graduation changed")

    def test_failure_cost_disagreements_cannot_be_erased(self):
        self.mutate(lambda cases: cases.pop(), "reference disagreements must remain visible", builtin.AUDIT_CORPUS)
        self.mutate(lambda cases: cases.pop(), "remaining deferred coverage", builtin.UNSUPPORTED_CORPUS)

    def test_budget_boundary_cannot_be_relaxed(self):
        def change(cases):
            case = next(case for case in cases if case["id"].endswith("/budget-cpu-minus-one"))
            case["mode"]["budget"]["cpu"] = case["provenance"]["budget_boundary"]["required_success_budget"]["cpu"]
        self.mutate(change, "budget boundary limit")

    def test_application_charge_cutoff_is_derived_and_cannot_be_relaxed(self):
        selected = {case["id"]: case for case in self.cases if "/probe/charge-" in case["id"]}
        self.assertEqual(len(selected), 4)
        for case in selected.values():
            expected = {"cpu": "101308", "mem": "102"} if "/charge-add-" in case["id"] else {"cpu": "76149", "mem": "101"}
            self.assertEqual(case["expected"]["budget"], expected)
            self.assertEqual(case["expected"]["kind"], "budget_exhausted")
        def change(cases):
            case = next(case for case in cases if "/probe/charge-add-cpu" in case["id"])
            case["mode"]["budget"]["cpu"] = "181308"
        self.mutate(change, "budget boundary limit")

    def test_publish_refuses_overwrite(self):
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / "corpus.jsonl"
            publish(path, b"independent\n")
            with self.assertRaises(FileExistsError):
                publish(path, b"replaced\n")
            self.assertEqual(path.read_bytes(), b"independent\n")
            self.assertEqual(list(Path(temp).iterdir()), [path])


if __name__ == "__main__":
    unittest.main()
