#!/usr/bin/env python3
"""Run persistent JSONL evaluators against shared cases; default policy is strict."""
import argparse
from collections import Counter
import hashlib
import itertools
import json
from pathlib import Path
import queue
import shlex
import subprocess
import threading
import time

ROOT = Path(__file__).resolve().parents[1]
MAX_RESPONSE = 16 * 1024 * 1024


def load_cases(paths):
    cases, ids = [], set()
    for path in paths:
        for number, line in enumerate(Path(path).read_text().splitlines(), 1):
            if not line.strip():
                continue
            case = json.loads(line)
            if case["id"] in ids:
                raise ValueError(f"duplicate case ID {case['id']} at {path}:{number}")
            ids.add(case["id"])
            if "expected" in case and case["expected"].get("status") not in ("success", "failure"):
                raise ValueError("goldens must describe a semantic success or failure")
            cases.append(case)
    if not cases:
        raise ValueError("the corpus is empty")
    return cases


def make_request(case):
    profile_path = (ROOT / case["profile"]).resolve()
    profile_path.relative_to(ROOT)
    profile = json.loads(profile_path.read_text())
    parameters = profile["cost_model"]["parameters"]
    if not parameters or any(type(p) is not str or str(int(p)) != p for p in parameters):
        raise ValueError("cost-model parameters must be canonical decimal strings")
    digest = hashlib.sha256(("[" + ",".join(parameters) + "]").encode()).hexdigest()
    if digest != profile["cost_model"]["sha256"]:
        raise ValueError("cost-model hash mismatch")
    # Profile provenance is retained on disk and in artifacts, outside the wire schema.
    profile = {key: profile[key] for key in ("id", "language", "protocol_major", "cost_model")}
    return {"schema_version": 1, "id": case["id"], "program": case["program"],
            "profile": profile, "mode": case["mode"]}


def compare(left, right, *, compare_failure_costs=False):
    """Return a category and explanation. Unsupported is never agreement."""
    statuses = {left["status"], right["status"]}
    if "infrastructure_error" in statuses:
        return "error", "an engine or its transport failed"
    if "unsupported" in statuses:
        return "unsupported", "at least one engine does not support this case"
    if left["status"] != right["status"]:
        return "mismatch", "success/failure differs"
    if left["status"] == "success":
        fields = ("term", "budget", "traces")
    elif left["status"] == "failure":
        fields = ("kind", "traces") + (("budget",) if compare_failure_costs else ())
    else:
        return "error", "unknown outcome status"
    differences = [field for field in fields if left.get(field) != right.get(field)]
    return ("mismatch", "different " + ", ".join(differences)) if differences else ("pass", "")


def compare_golden(outcome, expected):
    if outcome["status"] in ("unsupported", "infrastructure_error"):
        return compare(outcome, outcome)
    # Goldens may intentionally omit traces or failure costs not provided upstream.
    differences = [key for key, value in expected.items() if outcome.get(key) != value]
    return ("mismatch", "golden differs: " + ", ".join(differences)) if differences else ("pass", "")


class Engine:
    def __init__(self, name, command, timeout):
        self.name, self.command, self.timeout = name, command, timeout
        self.process = None
        self.responses = queue.Queue(maxsize=4)

    def start(self):
        self.process = subprocess.Popen(shlex.split(self.command), stdin=subprocess.PIPE,
                                        stdout=subprocess.PIPE, stderr=None, cwd=ROOT)
        process, responses = self.process, self.responses

        def read():
            try:
                while True:
                    line = process.stdout.readline(MAX_RESPONSE + 1)
                    responses.put(line, timeout=self.timeout)
                    if not line or len(line) > MAX_RESPONSE:
                        break
            except (OSError, ValueError, queue.Full):
                pass
        threading.Thread(target=read, daemon=True).start()

    def evaluate(self, request):
        try:
            if self.process is None:
                self.start()
            encoded = json.dumps(request, separators=(",", ":")).encode() + b"\n"
            # A worker may stop reading stdin as well as stdout. Bound both operations.
            errors = queue.Queue()
            process = self.process
            def send():
                try:
                    process.stdin.write(encoded)
                    process.stdin.flush()
                except (OSError, ValueError) as error:
                    errors.put(error)
            thread = threading.Thread(target=send, daemon=True)
            thread.start()
            deadline = time.monotonic() + self.timeout
            thread.join(self.timeout)
            if thread.is_alive():
                raise TimeoutError("engine did not read the request")
            if not errors.empty():
                raise errors.get()
            line = self.responses.get(timeout=max(0, deadline - time.monotonic()))
            if not line:
                raise RuntimeError("engine exited without a response")
            if len(line) > MAX_RESPONSE:
                raise RuntimeError("engine exceeded the response size limit")
            result = json.loads(line)
            if result["schema_version"] != 1 or result["id"] != request["id"]:
                raise ValueError("engine returned the wrong schema or request ID")
            if not result.get("engine") or not result.get("revision"):
                raise ValueError("engine identity/revision is missing")
            validate_outcome(result["outcome"])
            return result
        except (OSError, ValueError, KeyError, TypeError, RuntimeError, queue.Empty) as error:
            self.close()
            self.responses = queue.Queue(maxsize=4)
            return {"schema_version": 1, "id": request["id"], "engine": self.name,
                    "revision": "unavailable", "outcome": {"status": "infrastructure_error",
                    "diagnostic": str(error) or "engine timed out"}}

    def close(self):
        if self.process is not None:
            process, self.process = self.process, None
            if process.poll() is None:
                process.kill()
            process.wait()
            process.stdin.close()
            process.stdout.close()


def validate_outcome(outcome):
    status = outcome["status"]
    if status in ("success", "failure"):
        if not isinstance(outcome["traces"], list) or any(type(t) is not str for t in outcome["traces"]):
            raise ValueError("traces must be a list of strings")
        if status == "success":
            if not isinstance(outcome["term"], list) or not outcome["term"]:
                raise ValueError("success needs a normalized structural term")
            if outcome["budget"] is None:
                raise ValueError("success needs exact execution units")
        elif outcome["kind"] not in ("decode", "evaluation", "budget_exhausted"):
            raise ValueError("unknown failure kind")
        if outcome["budget"] is not None:
            for field in ("cpu", "mem"):
                value = outcome["budget"][field]
                if type(value) is not str or str(int(value)) != value or int(value) < 0:
                    raise ValueError("execution units must be nonnegative decimal strings")
    elif status not in ("unsupported", "infrastructure_error"):
        raise ValueError("unknown outcome status")


def run(args):
    cases = load_cases(args.corpus)
    engines = []
    for specification in args.engine:
        name, command = specification.split("=", 1)
        if not name or not command or any(engine.name == name for engine in engines):
            raise ValueError("engine names must be nonempty and unique")
        engines.append(Engine(name, command, args.timeout))
    counts = Counter()
    artifacts = Path(args.artifacts)
    artifacts.mkdir(parents=True, exist_ok=True)
    records = []
    try:
        for case in cases:
            request = make_request(case)
            responses = {engine.name: engine.evaluate(request) for engine in engines}
            checks = []
            if case.get("unavailable"):
                checks.append(("unsupported", case["unavailable"]))
            for name, response in responses.items():
                outcome = response["outcome"]
                if outcome["status"] in ("unsupported", "infrastructure_error"):
                    checks.append(compare(outcome, outcome))
                if "expected" in case:
                    checks.append(compare_golden(outcome, case["expected"]))
            for left, right in itertools.combinations(engines, 2):
                checks.append(compare(responses[left.name]["outcome"], responses[right.name]["outcome"],
                                      compare_failure_costs=args.failure_costs))
            if not checks:
                raise ValueError(f"{case['id']}: one engine without a golden provides no comparison")
            category = max((c for c, _ in checks), key={"pass": 0, "unsupported": 1, "mismatch": 2, "error": 3}.get)
            counts[category] += 1
            record = {"id": case["id"], "category": category, "checks": checks,
                      "request": request, "responses": responses, "case": case}
            records.append(record)
            if category in ("mismatch", "error"):
                name = hashlib.sha256(case["id"].encode()).hexdigest()[:16]
                (artifacts / f"{name}.json").write_text(json.dumps(record, indent=2) + "\n")
            if args.verbose or category != "pass":
                print(f"{category.upper():11} {case['id']}")
    finally:
        for engine in engines:
            engine.close()
    summary = {key: counts[key] for key in ("pass", "mismatch", "error", "unsupported")}
    summary["total"] = len(cases)
    summary["complete"] = counts["pass"] == len(cases)
    (artifacts / "report.json").write_text(json.dumps({"summary": summary, "cases": records}, indent=2) + "\n")
    print(json.dumps(summary, sort_keys=True))
    return int(bool(counts["mismatch"] or counts["error"] or (counts["unsupported"] and not args.allow_unsupported)))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--corpus", nargs="+", default=[str(ROOT / "fixtures/smoke.jsonl")])
    parser.add_argument("--engine", action="append", required=True, help="NAME=COMMAND; no shell is invoked")
    parser.add_argument("--timeout", type=float, default=10)
    parser.add_argument("--artifacts", default=str(ROOT / "artifacts/conformance"))
    parser.add_argument("--allow-unsupported", action="store_true", help="coverage report only; unsupported cases still never pass")
    parser.add_argument("--failure-costs", action="store_true", help="also compare partial costs on failures")
    parser.add_argument("--verbose", action="store_true")
    args = parser.parse_args()
    if args.timeout <= 0:
        parser.error("timeout must be positive")
    raise SystemExit(run(args))
