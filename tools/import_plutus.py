#!/usr/bin/env python3
"""Import a pinned Plutus corpus without regenerating its semantic/cost goldens."""
import argparse
import hashlib
import json
from pathlib import Path
import re

from conformance import Engine, ROOT, make_request


def main(args):
    pin = json.loads((ROOT / "upstreams.lock.json").read_text())["plutus"]
    checkout = ROOT / ".cache/upstreams/plutus"
    if json.loads((checkout / ".uplc-scaffold-source.json").read_text()) != pin:
        raise ValueError("Plutus source pin mismatch; run tools/upstreams.py")
    corpus = checkout / "plutus-conformance/test-cases/uplc/evaluation"
    paths = sorted(corpus.rglob("*.uplc"))
    if args.limit:
        paths = paths[:args.limit]
    if not paths:
        raise ValueError("no upstream cases found")
    normalizer = Engine("normalizer", args.normalizer + " --normalize", args.timeout)
    count = unavailable = 0
    args.output.parent.mkdir(parents=True, exist_ok=True)
    if args.output.exists():
        raise ValueError(f"refusing to overwrite {args.output}; choose a new output or remove it explicitly")
    try:
        with args.output.open("w") as output:
            for source in paths:
                expected_file = source.with_suffix(".uplc.expected")
                budget_file = source.with_suffix(".uplc.budget.expected")
                if not budget_file.exists():
                    budget_file = source.with_suffix(".budget.expected")
                expected = expected_file.read_text().strip()
                budget_text = budget_file.read_text().strip()
                case = {"id": "plutus/" + source.relative_to(corpus).as_posix(),
                        "profile": args.profile, "program": {"format": "uplc_text", "source": source.read_text()},
                        "mode": {"kind": "restricting", "budget": {"cpu": "1000000000000000", "mem": "1000000000000000"}},
                        "provenance": {"repository": pin["repository"], "revision": pin["revision"],
                                       "path": source.relative_to(checkout).as_posix(),
                                       "sha256": hashlib.sha256(source.read_bytes()).hexdigest()}}
                if expected in ("parse error", "parse/decode error", "evaluation failure"):
                    case["expected"] = {"status": "failure", "kind": "evaluation" if expected == "evaluation failure" else "decode"}
                else:
                    match = re.fullmatch(r"\(\{cpu:\s*(\d+)\s*\|\s*mem:\s*(\d+)\}\)", budget_text)
                    if not match:
                        raise ValueError(f"unrecognized budget golden: {budget_file}")
                    case["expected"] = {"status": "success", "budget": {"cpu": match[1], "mem": match[2]}}
                    request = make_request(case)
                    request["program"] = {"format": "uplc_text", "source": expected}
                    result = normalizer.evaluate(request)["outcome"]
                    if result["status"] == "success":
                        case["expected"]["term"] = result["term"]
                    elif result["status"] == "infrastructure_error":
                        raise RuntimeError("golden normalizer failed: " + json.dumps(result))
                    else:
                        case["unavailable"] = "golden term could not be normalized: " + json.dumps(result)
                        unavailable += 1
                output.write(json.dumps(case, separators=(",", ":")) + "\n")
                count += 1
    finally:
        normalizer.close()
    print(json.dumps({"imported": count, "unavailable": unavailable, "revision": pin["revision"],
                      "note": "restricting runs of counting-mode goldens; budget exhaustion remains a reported difference"}))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--normalizer", default="tools/oracle-aiken/target/debug/oracle-aiken")
    parser.add_argument("--profile", default="profiles/plutus-v3-pv11.json")
    parser.add_argument("--output", type=Path, default=ROOT / ".cache/plutus.jsonl")
    parser.add_argument("--limit", type=int)
    parser.add_argument("--timeout", type=float, default=10)
    main(parser.parse_args())
