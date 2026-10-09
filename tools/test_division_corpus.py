"""Independent division mathematics, charge ledgers, and provenance mutations."""
import copy
import json
from pathlib import Path
import shutil
import tempfile
import unittest

import build_division_corpus as division
from build_milestone_corpus import encode_jsonl, sha256
from conformance import ROOT, load_cases
from generate_cases import execution_ledger, generate


class DivisionDerivationTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.parameters = json.loads((ROOT / division.PROFILE).read_text())["cost_model"]["parameters"]

    def test_sign_table_is_floor_and_truncation_with_matching_residues(self):
        # Pinned cardano/builtins1.tex sign table, independently written values.
        for a, b, expected in ((7, 3, (2, 2, 1, 1)), (-7, 3, (-3, -2, -1, 2)),
                               (7, -3, (-3, -2, 1, -2)), (-7, -3, (2, 2, -1, -1)),
                               (2, -7, (-1, 0, 2, -5)), (-2, 7, (-1, 0, -2, 5)),
                               (0, -1, (0, 0, 0, 0)), (-6, 3, (-2, -2, 0, 0))):
            actual = tuple(division.division_result(name, a, b) for name in division.DIVISION)
            self.assertEqual(actual, expected)
            floor, quotient, remainder, modulo = actual
            self.assertEqual(a, b * floor + modulo)
            self.assertEqual(a, b * quotient + remainder)
            self.assertTrue(not remainder or (remainder > 0) == (a > 0))
            self.assertTrue(not modulo or (modulo > 0) == (b > 0))

    def test_large_integer_mathematics_never_uses_float(self):
        a, b = -(2**8192 + 19), 2**129 + 7
        q = division.division_result("quotientInteger", a, b)
        r = division.division_result("remainderInteger", a, b)
        self.assertEqual(a, q*b+r)
        self.assertLess(r, 0)
        self.assertLess(abs(r), b)
        self.assertEqual(division.division_result("divideInteger", a, b), q-1)
        self.assertEqual(division.division_result("modInteger", a, b), r+b)
        for name in division.DIVISION:
            with self.assertRaises(ZeroDivisionError):
                division.division_result(name, a, 0)

    def test_decimal_bounds_do_not_change_python_process_policy(self):
        for n in (0, -1, 2**53+1, -(2**262143), 2**262143-1):
            text = division.decimal_text(n)
            self.assertEqual(division.decimal_integer(text), n)
        with self.assertRaisesRegex(ValueError, "portable magnitude"):
            division.decimal_integer("1" + "0" * 157828)

    def test_actual_profile_known_polynomial_and_asymmetric_branches(self):
        for name in division.DIVISION:
            actual = division.builtin_budget(name, [str(2**128), str(2**64)], self.parameters)
            self.assertEqual(actual, {"cpu": "145634", "mem": "1" if name in ("divideInteger", "quotientInteger") else "2"})
            actual = division.builtin_budget(name, ["1", str(2**192)], self.parameters)
            self.assertEqual(actual, {"cpu": "141224" if name in ("divideInteger", "modInteger") else "85848", "mem": "1" if name in ("divideInteger", "quotientInteger") else "4"})

    def test_signed_minimum_is_inside_memory_affine_expression(self):
        for name in ("divideInteger", "quotientInteger"):
            p = ["0"] * 350
            i = division.DIVISION_COST_INDICES[name]
            p[i+8:i+11] = ["13", "-2", "-3"]
            # Signed x-y = -3, max(-2,-3)=-2, then 13-3*(-2)=19.
            self.assertEqual(division.builtin_budget(name, ["1", str(2**192)], p)["mem"], "19")
            self.assertEqual(division.builtin_budget(name, [str(2**192), "1"], p)["mem"], "4")

    def test_ignored_constants_minima_and_exact_cancellation(self):
        for name in division.DIVISION:
            i = division.DIVISION_COST_INDICES[name]
            p = ["0"] * 350
            p[i:i+8] = ["-999", "-100", "-7", "-2", "-11", "-3", "-5", "97"]
            self.assertEqual(division.builtin_budget(name, [str(2**64), str(2**64)], p)["cpu"], "97")
            smaller = division.builtin_budget(name, ["1", str(2**192)], p)["cpu"]
            self.assertEqual(smaller, "97" if name in ("divideInteger", "modInteger") else "-999")
            p[i:i+8] = ["0", "0", "0", str(-(2**63-1)), "0", "0", str(2**63-1), "0"]
            self.assertEqual(division.builtin_budget(name, [str(2**64), str(2**64)], p)["cpu"], "0")

    def test_zero_failure_is_charged_without_pending_flush(self):
        for name in division.DIVISION:
            events = ["apply", "apply", "builtin", "constant", "constant", {"builtin": name, "arguments": ["1", "0"]}, "error"]
            actual = division.ledger_outcome(events, self.parameters, division.LIMIT)
            self.assertEqual(actual, {"status": "failure", "kind": "evaluation", "budget": {"cpu": "132441", "mem": "101"}, "traces": []})
            for limit in ({"cpu": "132440", "mem": "1000000"}, {"cpu": "1000000", "mem": "100"}):
                actual = division.ledger_outcome(events, self.parameters, limit)
                self.assertEqual(actual["kind"], "budget_exhausted")
                self.assertEqual(actual["budget"], {"cpu": "132441", "mem": "101"})

    def test_batch_failure_precedes_denotation_at_event_200(self):
        events = ["force", "delay"] * 98 + ["apply", "apply", "builtin", "constant", "constant", {"builtin": "divideInteger", "arguments": ["1", "0"]}, "error"]
        actual = division.ledger_outcome(events, self.parameters, division.LIMIT)
        self.assertEqual(actual["budget"], {"cpu": "3332441", "mem": "20101"})
        actual = division.ledger_outcome(events, self.parameters, {"cpu": "100", "mem": "100"})
        self.assertEqual(actual["kind"], "budget_exhausted")
        self.assertEqual(actual["budget"], {"cpu": "16100", "mem": "200"})

    def test_generated_division_is_reproducible_exact_and_failure_aware(self):
        cases = list(generate(42, 1000, division=True))
        self.assertEqual(cases, list(generate(42, 1000, division=True)))
        names = set()
        failed = 0
        for case in cases:
            arithmetic = case["provenance"]["arithmetic"]
            name = arithmetic["builtin"]
            if name not in division.DIVISION:
                continue
            names.add(name)
            a, b = map(int, arithmetic["arguments"])
            ledger = execution_ledger(arithmetic)
            if b:
                self.assertEqual(case["expected"]["term"], ["constant", ["integer", str(division.division_result(name, a, b))]])
                self.assertEqual(ledger[-1], "halt")
            else:
                failed += 1
                self.assertEqual(case["expected"]["status"], "failure")
                self.assertEqual(ledger[-1], "error")
                self.assertNotIn("var", ledger)
        self.assertEqual(names, set(division.DIVISION))
        self.assertGreater(failed, 10)

    def test_old_generator_seed_output_is_unchanged(self):
        # Captured from b225423's original text generator, before extension.
        import hashlib
        data = "".join(json.dumps(case, separators=(",", ":"), sort_keys=True) + "\n"
                       for case in generate(42, 1000)).encode()
        self.assertEqual(hashlib.sha256(data).hexdigest(), "47d694496e46053ab201d637c257d6bfe17419f7dbe63ad32927e7f9b3e35773")


class DivisionProvenanceTests(unittest.TestCase):
    def copy_tree(self, root):
        for path in ("upstreams.lock.json", division.PROFILE, division.CORPUS,
                     division.CANDIDATE_CORPUS, division.DECODER_CORPUS,
                     division.AUDIT_CORPUS, division.UNSUPPORTED_CORPUS,
                     "fixtures/builtins-unsupported.jsonl"):
            destination = root / path
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(ROOT / path, destination)
        shutil.copytree(ROOT / "fixtures/division", root / "fixtures/division")

    def mutate(self, change, match, corpus=division.CORPUS):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            self.copy_tree(root)
            cases = load_cases([root / corpus])
            change(cases)
            (root / corpus).write_bytes(encode_jsonl(cases))
            with self.assertRaisesRegex(ValueError, match):
                division.verify_committed(root)

    def test_selection_and_original_regressions(self):
        counts = division.verify_committed()
        self.assertEqual(counts["official"], 24)
        self.assertEqual(counts["graduated"], 5)
        self.assertEqual(counts["decoder"], 16)
        self.assertEqual(counts["unsupported"], 21)
        from build_builtin_corpus import verify_committed
        self.assertEqual(verify_committed(), {"builtins": 162, "decoder": 8, "candidate": 24, "audit": 24, "unsupported": 14, "official": 39, "graduated": 1})

    def test_all_four_input_bounds_have_independent_cases(self):
        manifest = json.loads((ROOT / division.MANIFEST).read_text())
        probes = {probe["id"]: probe for probe in manifest["probes"]}
        boundary = 1 << 262143
        for name in division.DIVISION:
            for position in (0, 1):
                for label, endpoint in (("minimum", -boundary), ("maximum", boundary-1)):
                    probe = probes[name + f"-bound-{label}-arg{position}"]
                    self.assertEqual(division.decimal_integer(probe["arithmetic"]["arguments"][position]), endpoint)
                    self.assertFalse(probe.get("candidate_only"))
                for label in ("below-minimum", "above-maximum"):
                    probe = probes[name + f"-bound-{label}-arg{position}"]
                    self.assertTrue(probe["candidate_only"])
                    self.assertTrue(probe["reference_disagreement"])
                    self.assertFalse(any(isinstance(event, dict) for event in probe["events"]))
            probe = probes[name + "-bound-minimum-arg0"]
            self.assertEqual(probe["arithmetic"]["arguments"][1], "-1")
            expected = boundary if name in ("divideInteger", "quotientInteger") else 0
            self.assertEqual(probe["term"], ["constant", ["integer", division.decimal_text(expected)]])

    def test_raw_official_bytes_and_golden_costs_are_authoritative(self):
        def mutate(cases):
            record = cases[0]["provenance"]["goldens"]["result"]
            record["text"] += "\n"
            record["sha256"] = sha256(record["text"].encode())
        self.mutate(mutate, "raw provenance mismatch")
        self.mutate(lambda cases: cases[0]["expected"]["budget"].update(cpu="0"), "expected result or budget")

    def test_two_pinned_encoders_and_parsers_must_agree(self):
        def mutate(cases):
            cases[0]["program"]["hex"] += "00"
            cases[0]["provenance"]["flat_sha256"] = sha256(bytes.fromhex(cases[0]["program"]["hex"]))
        self.mutate(mutate, "Flat bytes disagree")
        def mutate(cases):
            cases[0]["provenance"]["normalizers"][1]["engine"] = "aiken-normalizer"
        self.mutate(mutate, "both pinned normalizer")
        def mutate(cases):
            cases[0]["provenance"]["normalizers"][1]["outcome"]["term"] = ["error"]
        self.mutate(mutate, "normalizer disagreement")

    def test_graduations_and_failing_audits_cannot_be_erased(self):
        def mutate(cases):
            case = next(case for case in cases if "graduation" in case["provenance"])
            case["provenance"]["graduation"]["case_sha256"] = "0" * 64
        self.mutate(mutate, "graduation changed")
        self.mutate(lambda cases: cases.pop(), "reference disagreements must remain visible", division.AUDIT_CORPUS)
        self.mutate(lambda cases: cases.pop(), "remaining deferred coverage", division.UNSUPPORTED_CORPUS)

    def test_decoder_request_metadata_is_bound_to_manifest(self):
        for key, value in (("id", "altered"), ("mode", {"kind": "counting"}),
                           ("profile", "fixtures/division/profiles/zero.json")):
            self.mutate(lambda cases: cases[0].update({key: value}), "decoder request differs", division.DECODER_CORPUS)

    def test_new_unsupported_metadata_and_sources_cannot_change(self):
        def select(cases):
            return next(case for case in cases if case["id"].startswith("division/"))
        for key, value in (("id", "altered"), ("mode", {"kind": "counting"}),
                           ("profile", division.PROFILE)):
            self.mutate(lambda cases: select(cases).update({key: value}), "unsupported request differs", division.UNSUPPORTED_CORPUS)
        self.mutate(lambda cases: select(cases)["provenance"]["source"].update(path="altered"),
                    "unsupported derivation source path", division.UNSUPPORTED_CORPUS)

    def test_all_corpora_validate_full_envelopes_against_supplied_root(self):
        for corpus in (division.CORPUS, division.CANDIDATE_CORPUS, division.DECODER_CORPUS,
                       division.UNSUPPORTED_CORPUS, division.AUDIT_CORPUS):
            self.mutate(lambda cases: cases[0]["program"].update(unexpected=True), "expected fields", corpus)
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            self.copy_tree(root)
            self.assertEqual(division.verify_committed(root), division.verify_committed())
            profile = json.loads((root / division.PROFILE).read_text())
            profile["protocol_major"] = 65536
            (root / division.PROFILE).write_text(json.dumps(profile))
            manifest = json.loads((root / division.MANIFEST).read_text())
            for path, custom in division.custom_profiles(manifest, profile).items():
                (root / path).write_text(json.dumps(custom))
            with self.assertRaisesRegex(ValueError, "protocol_major must be a u16"):
                division.verify_committed(root)

    def test_profiles_cannot_escape_verification_root(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            self.copy_tree(root)
            profile = root / division.PROFILE
            profile.unlink()
            profile.symlink_to(ROOT / division.PROFILE)
            with self.assertRaises(ValueError):
                division.verify_committed(root)

    def test_budget_boundary_cannot_be_relaxed(self):
        def mutate(cases):
            case = next(case for case in cases if case["id"].endswith("/budget-cpu-minus-one"))
            case["mode"]["budget"]["cpu"] = str(int(case["mode"]["budget"]["cpu"]) + 1)
        self.mutate(mutate, "budget boundary limit")


if __name__ == "__main__":
    unittest.main()
