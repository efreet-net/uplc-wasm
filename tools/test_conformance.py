import contextlib
import io
import json
import os
import re
from pathlib import Path
import shlex
import sys
import tempfile
import time
import unittest
from argparse import Namespace

from conformance import Engine, ROOT, compare, compare_golden, load_cases, make_request, run, validate_outcome
from protocol import MAX_REQUEST, loads, validate_request, validate_response, validate_term


def success(value="9007199254740993", cpu="123"):
    return {"status": "success", "term": ["constant", ["integer", value]],
            "budget": {"cpu": cpu, "mem": "10"}, "traces": []}


class ComparisonTests(unittest.TestCase):
    def test_result_cost_and_trace_disagreements_are_independent(self):
        original = success()
        for field, value in [("term", ["constant", ["integer", "9007199254740992"]]),
                             ("budget", {"cpu": "124", "mem": "10"}), ("traces", ["changed"])]:
            other = dict(original, **{field: value})
            self.assertEqual(compare(original, other)[0], "mismatch")

    def test_identical_unsupported_or_crashed_engines_never_pass(self):
        for status, category in [("unsupported", "unsupported"), ("infrastructure_error", "error")]:
            self.assertEqual(compare({"status": status}, {"status": status})[0], category)

    def test_error_phase_and_exhaustion_are_not_collapsed(self):
        error = {"status": "failure", "kind": "evaluation", "budget": None, "traces": []}
        for kind in ("decode", "budget_exhausted"):
            self.assertEqual(compare(error, dict(error, kind=kind))[0], "mismatch")

    def test_failure_budgets_are_an_explicit_policy(self):
        error = {"status": "failure", "kind": "evaluation", "budget": None, "traces": []}
        other = dict(error, budget={"cpu": "1", "mem": "1"})
        self.assertEqual(compare(error, other)[0], "pass")
        self.assertEqual(compare(error, other, compare_failure_costs=True)[0], "mismatch")

    def test_partial_golden_does_not_invent_missing_traces(self):
        self.assertEqual(compare_golden(success(), {"status": "success", "term": success()["term"]})[0], "pass")

    def test_budget_numbers_cannot_arrive_as_javascript_numbers(self):
        bad = success()
        bad["budget"]["cpu"] = 9007199254740993
        with self.assertRaises(ValueError):
            validate_outcome(bad)

    def test_smoke_cases_use_the_full_explicit_profile(self):
        cases = load_cases([ROOT / "fixtures/smoke.jsonl"])
        self.assertGreater(len(cases), 0)
        request = make_request(cases[0])
        self.assertEqual(len(request["profile"]["cost_model"]["parameters"]), 350)

    def test_adapter_identities_match_source_pins(self):
        pins = json.loads((ROOT / "upstreams.lock.json").read_text())
        for name in ("aiken", "amaru"):
            source = (ROOT / f"tools/oracle-{name}/src/main.rs").read_text()
            revision = re.search(r'const REVISION: &str = "([0-9a-f]{40})"', source)[1]
            self.assertEqual(revision, pins[name]["revision"])


class ValidationTests(unittest.TestCase):
    def test_malformed_terms_cannot_create_false_agreement(self):
        for term in (["unknown"], ["constant", ["integer", 9007199254740993]],
                     ["constant", ["bool", 1]], ["var", "+1"], ["lambda"],
                     ["constant", ["bytes", "FF"]], ["constr", "0", {}],
                     ["constant", ["list", "unknown-type", []]],
                     ["constant", ["list", "integer", [["bool", True]]]],
                     ["constant", ["string", "\ud800"]],
                     ["constant", ["pair", "integer", "unit", ["integer", "01"], ["unit"]]]):
            with self.subTest(term=term), self.assertRaises(ValueError):
                validate_outcome(dict(success(), term=term))

    def test_full_structures_and_large_integer_strings(self):
        validate_term(["case", ["constr", "0", [["constant", ["unit"]]]],
                       [["lambda", ["apply", ["builtin", "0"], ["var", "1"]]]]])
        validate_term(["constant", ["pair", ["list", "integer"], "bool",
                       ["list", "integer", [["integer", "9" * 10000]]], ["bool", True]]])
        term = ["error"]
        for _ in range(513):
            term = ["delay", term]
        with self.assertRaisesRegex(ValueError, "depth"):
            validate_term(term)

    def test_outcomes_require_all_fields_and_canonical_budgets(self):
        for outcome in ({"status": "unsupported"}, {"status": "infrastructure_error"},
                        dict(success(), extra=True), dict(success(), budget={"cpu": str(2**63), "mem": "0"}),
                        dict(success(), traces=[1]), dict(success(), budget=None)):
            with self.subTest(outcome=outcome), self.assertRaises(ValueError):
                validate_outcome(outcome)

    def test_strict_json_and_response_envelope(self):
        for text in ('{"x":1,"x":2}', '{"x":NaN}', '{"x":Infinity}'):
            with self.assertRaises(ValueError):
                loads(text)
        valid = dict(schema_version=1, id="test", engine="test", revision="test", outcome=success())
        for response in (dict(valid, schema_version=True), dict(valid, engine=1), dict(valid, extra=1)):
            with self.assertRaises(ValueError):
                validate_response(response, "test")

    def test_request_validation_matches_rust_limits(self):
        original = make_request(load_cases([ROOT / "fixtures/smoke.jsonl"])[0])
        for field, value in (("schema_version", True), ("mode", {"kind": "counting", "budget": {}}),
                             ("program", {"format": "flat", "hex": "00 11"})):
            with self.assertRaises(ValueError):
                validate_request(dict(original, **{field: value}))
        for coefficient in ("01", "-0", str(2**63), "1" * 10000):
            request = json.loads(json.dumps(original))
            request["profile"]["cost_model"]["parameters"][0] = coefficient
            with self.assertRaises(ValueError):
                validate_request(request)


class ProcessTests(unittest.TestCase):
    def engine(self, script, timeout=1):
        return Engine("test", f"{shlex.quote(sys.executable)} -u -c {shlex.quote(script)}", timeout)

    def test_hung_process_is_infrastructure_error(self):
        engine = self.engine("import time; time.sleep(30)", timeout=0.15)
        try:
            self.assertEqual(engine.evaluate({"id": "hang"})["outcome"]["status"], "infrastructure_error")
        finally:
            engine.close()

    def test_crash_is_not_semantic_failure(self):
        engine = self.engine("raise SystemExit(9)")
        try:
            self.assertEqual(engine.evaluate({"id": "crash"})["outcome"]["status"], "infrastructure_error")
        finally:
            engine.close()

    @unittest.skipUnless(os.name == "posix", "process groups require POSIX")
    def test_descendant_holding_pipes_cannot_extend_deadline(self):
        engine = self.engine("import subprocess,sys\nsubprocess.Popen([sys.executable,'-c','import time; time.sleep(3)'])", timeout=0.15)
        started = time.monotonic()
        try:
            self.assertEqual(engine.evaluate({"id": "child"})["outcome"]["status"], "infrastructure_error")
            self.assertLess(time.monotonic() - started, 1.5)
        finally:
            engine.close()

    def test_blocked_stdin_is_bounded(self):
        engine = self.engine("import time; time.sleep(3)", timeout=0.15)
        started = time.monotonic()
        try:
            result = engine.evaluate({"id": "blocked-write", "payload": "x" * 1000000})
            self.assertEqual(result["outcome"]["status"], "infrastructure_error")
            self.assertLess(time.monotonic() - started, 1.5)
        finally:
            engine.close()

    def test_request_size_limit_excludes_the_framing_lf(self):
        response = dict(schema_version=1, id="size-limit", engine="test", revision="test", outcome=success())
        script = "import sys\nfor line in sys.stdin.buffer:\n print(" + repr(json.dumps(response)) + ",flush=True)"
        engine = self.engine(script, timeout=3)
        request = {"id": "size-limit", "payload": "λ"}
        # The limit applies to bytes of the actual serialized JSON, including
        # the runner's Unicode escapes, not to the Python payload's length.
        serialized = json.dumps(request, separators=(",", ":")).encode()
        request["payload"] += " " * (MAX_REQUEST - len(serialized))
        self.assertEqual(len(json.dumps(request, separators=(",", ":")).encode()), MAX_REQUEST)
        try:
            self.assertEqual(engine.evaluate(request)["outcome"], success())
            request["payload"] += " "
            result = engine.evaluate(request)["outcome"]
            self.assertEqual(result["status"], "infrastructure_error")
            self.assertEqual(result["diagnostic"], "request exceeds the transport size limit")
            self.assertIsNone(engine.process)
        finally:
            engine.close()

    def test_malformed_response_and_restart(self):
        engine = self.engine("import sys; sys.stdin.readline(); print('{\"schema_version\":' + '['*2000 + '0' + ']'*2000 + '}')")
        try:
            for _ in range(2):
                self.assertEqual(engine.evaluate({"id": "malformed"})["outcome"]["status"], "infrastructure_error")
        finally:
            engine.close()

    def test_unterminated_jsonl_is_an_error(self):
        response = dict(schema_version=1, id="test", engine="test", revision="test", outcome=success())
        engine = self.engine("import sys; sys.stdin.readline(); sys.stdout.write(" + repr(json.dumps(response)) + ")")
        try:
            self.assertEqual(engine.evaluate({"id": "test"})["outcome"]["status"], "infrastructure_error")
        finally:
            engine.close()

    def test_nonfinite_deadlines_are_rejected(self):
        for timeout in (float("nan"), float("inf"), 0, -1):
            with self.assertRaises(ValueError):
                self.engine("pass", timeout)

    def test_references_are_compared_when_candidate_is_unsupported(self):
        case = json.loads((ROOT / "fixtures/smoke.jsonl").read_text().splitlines()[0])
        case.pop("expected")
        def command(outcome):
            script = "import sys,json\nfor line in sys.stdin:\n r=json.loads(line); print(json.dumps(dict(schema_version=1,id=r['id'],engine='test',revision='test',outcome=" + repr(outcome) + ")),flush=True)"
            return f"{shlex.quote(sys.executable)} -u -c {shlex.quote(script)}"
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / "cases.jsonl"
            path.write_text(json.dumps(case) + "\n")
            args = Namespace(corpus=[path], engine=["candidate=" + command({"status": "unsupported", "reason": "stub"}),
                             "a=" + command(success("1")), "b=" + command(success("2"))],
                             timeout=1, artifacts=temp, failure_costs=False, allow_unsupported=True, verbose=False)
            with contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(run(args), 1)
            report = json.loads((Path(temp) / "report.json").read_text())
            self.assertEqual(report["summary"]["mismatch"], 1)


if __name__ == "__main__":
    unittest.main()
