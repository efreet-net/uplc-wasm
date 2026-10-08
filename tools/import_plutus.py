#!/usr/bin/env python3
"""Import a pinned Plutus corpus without regenerating its semantic/cost goldens."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import tempfile

from conformance import Engine, ROOT, make_request
from upstreams import verify_source


def main(args):
    pin = json.loads((ROOT / "upstreams.lock.json").read_text())["plutus"]
    checkout = verify_source("plutus", pin)
    profile = json.loads((ROOT / args.profile).read_text())
    if (profile["language"], profile["protocol_major"], profile.get("provenance", {}).get("plutus_revision")) != ("PlutusV3", 11, pin["revision"]):
        raise ValueError("the importer requires the pinned PlutusV3/PV11 fixture profile")
    canonical = json.loads((ROOT / "profiles/plutus-v3-pv11.json").read_text())
    if profile["cost_model"] != canonical["cost_model"]:
        raise ValueError("the imported cost goldens require the exact pinned fixture cost model")
    corpus = checkout / "plutus-conformance/test-cases/uplc/evaluation"
    paths = sorted(corpus.rglob("*.uplc"))
    if args.limit is not None:
        if args.limit < 1:
            raise ValueError("limit must be positive")
        paths = paths[:args.limit]
    if not paths:
        raise ValueError("no upstream cases found")
    commands = args.normalizer or ["tools/oracle-aiken/target/debug/oracle-aiken",
                                   "tools/oracle-amaru/target/debug/oracle-amaru"]
    if len(commands) < 2 or len(set(commands)) != len(commands):
        raise ValueError("at least two distinct golden normalizers are required")
    normalizers = [Engine(f"normalizer-{i}", command + " --normalize", args.timeout)
                   for i, command in enumerate(commands)]
    count = unavailable = 0
    args.output.parent.mkdir(parents=True, exist_ok=True)
    if args.output.exists():
        raise ValueError(f"refusing to overwrite {args.output}; choose a new output or remove it explicitly")
    temporary = None
    try:
        with tempfile.NamedTemporaryFile(mode="w", encoding="utf-8", dir=args.output.parent, delete=False) as output:
            temporary = Path(output.name)
            for source in paths:
                expected_file = source.with_suffix(".uplc.expected")
                budget_file = source.with_suffix(".uplc.budget.expected")
                if not budget_file.exists():
                    budget_file = source.with_suffix(".budget.expected")
                source_bytes = source.read_bytes()
                expected_bytes = expected_file.read_bytes()
                budget_bytes = budget_file.read_bytes()
                expected_text = expected_bytes.decode("utf-8")
                expected = expected_text.strip()
                budget_raw = budget_bytes.decode("utf-8")
                budget_text = budget_raw.strip()
                case = {"id": "plutus/" + source.relative_to(corpus).as_posix(),
                        "profile": args.profile, "program": {"format": "uplc_text", "source": source_bytes.decode("utf-8")},
                        "mode": {"kind": "restricting", "budget": {"cpu": "1000000000000000", "mem": "1000000000000000"}},
                        "provenance": {"repository": pin["repository"], "revision": pin["revision"],
                                       "path": source.relative_to(checkout).as_posix(),
                                       "sha256": hashlib.sha256(source_bytes).hexdigest(),
                                       "goldens": {"result": {"path": expected_file.relative_to(checkout).as_posix(),
                                                               "sha256": hashlib.sha256(expected_bytes).hexdigest(),
                                                               "text": expected_text},
                                                   "budget": {"path": budget_file.relative_to(checkout).as_posix(),
                                                              "sha256": hashlib.sha256(budget_bytes).hexdigest(),
                                                              "text": budget_raw}}}}
                if expected in ("parse error", "parse/decode error", "evaluation failure"):
                    if budget_text != expected:
                        raise ValueError(f"inconsistent failure and budget goldens: {source}")
                    case["expected"] = {"status": "failure", "kind": "evaluation" if expected == "evaluation failure" else "decode"}
                else:
                    match = re.fullmatch(r"\(\{cpu:\s*(\d+)\s*\|\s*mem:\s*(\d+)\}\)", budget_text)
                    if not match:
                        raise ValueError(f"unrecognized budget golden: {budget_file}")
                    case["expected"] = {"status": "success", "budget": {"cpu": match[1], "mem": match[2]}}
                    request = make_request(case)
                    request["program"] = {"format": "uplc_text", "source": expected}
                    responses = [normalizer.evaluate(request) for normalizer in normalizers]
                    # Retain each parser's output: parsing a golden can itself be wrong.
                    case["provenance"]["normalizers"] = [
                        {key: response[key] for key in ("engine", "revision", "outcome")}
                        for response in responses]
                    if len({r["engine"] for r in responses}) != len(responses):
                        raise ValueError("golden normalizers must identify distinct implementations")
                    results = [response["outcome"] for response in responses]
                    if any(result["status"] == "infrastructure_error" for result in results):
                        raise RuntimeError("golden normalizer failed: " + json.dumps(results))
                    if all(result["status"] == "success" for result in results) and all(result["term"] == results[0]["term"] for result in results[1:]):
                        case["expected"]["term"] = results[0]["term"]
                    else:
                        case["unavailable"] = "golden parsers could not agree on a normalized term; see provenance.normalizers"
                        unavailable += 1
                output.write(json.dumps(case, separators=(",", ":")) + "\n")
                count += 1
        # Publish a complete corpus atomically without overwriting an existing file.
        os.link(temporary, args.output)
    finally:
        for normalizer in normalizers:
            normalizer.close()
        if temporary is not None:
            temporary.unlink()
    print(json.dumps({"imported": count, "unavailable": unavailable, "revision": pin["revision"],
                      "note": "restricting runs of counting-mode goldens; budget exhaustion remains a reported difference"}))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--normalizer", action="append", help="repeat for two or more distinct parsers; defaults to Aiken and Amaru")
    parser.add_argument("--profile", default="profiles/plutus-v3-pv11.json")
    parser.add_argument("--output", type=Path, default=ROOT / ".cache/plutus.jsonl")
    parser.add_argument("--limit", type=int)
    parser.add_argument("--timeout", type=float, default=10)
    main(parser.parse_args())
