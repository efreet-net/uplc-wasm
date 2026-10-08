import contextlib
import io
import json
import re
from pathlib import Path
import shlex
import sys
import tempfile
import unittest
from argparse import Namespace

from conformance import Engine, ROOT, compare, compare_golden, load_cases, make_request, run, validate_outcome


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
