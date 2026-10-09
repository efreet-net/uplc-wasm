#!/usr/bin/env python3
"""Verify the profile and vendored goldens against content-checked pinned sources."""
import hashlib
import json
import re

from conformance import ROOT, load_cases, make_request
from upstreams import verify_source
from build_milestone_corpus import verify_committed
from build_builtin_corpus import verify_committed as verify_builtin_committed


def verify():
    pins = json.loads((ROOT / "upstreams.lock.json").read_text())
    aiken = verify_source("aiken", pins["aiken"])
    plutus = verify_source("plutus", pins["plutus"])
    profile = json.loads((ROOT / "profiles/plutus-v3-pv11.json").read_text())
    source = (aiken / "crates/uplc/tests/conformance.rs").read_text()
    costs = re.search(r"const V3_PV11_COSTS: &\[i64\] = &\[(.*?)\];", source, re.S)
    if costs is None or re.findall(r"-?\d+", costs[1]) != profile["cost_model"]["parameters"]:
        raise ValueError("profile differs from the pinned V3_PV11_COSTS vector")
    for upstream in ("aiken", "plutus"):
        if profile["provenance"][upstream + "_revision"] != pins[upstream]["revision"]:
            raise ValueError("profile provenance differs from upstream pins")
    checked = 0
    for case in load_cases([ROOT / "fixtures/smoke.jsonl"]):
        make_request(case)  # Includes canonical integers and the complete model hash.
        provenance = case["provenance"]
        if provenance.get("repository") != pins["plutus"]["repository"]:
            continue
        source = plutus / provenance["path"]
        if provenance["revision"] != pins["plutus"]["revision"] or hashlib.sha256(source.read_bytes()).hexdigest() != provenance["sha256"]:
            raise ValueError(f"seed provenance mismatch: {case['id']}")
        if case["program"] != {"format": "uplc_text", "source": source.read_text()}:
            raise ValueError(f"seed input differs from upstream: {case['id']}")
        relative = source.relative_to(plutus / "plutus-conformance/test-cases/uplc/evaluation")
        for suffix in ("", ".expected", ".budget.expected"):
            vendored = ROOT / "fixtures/plutus" / (str(relative) + suffix)
            if vendored.read_bytes() != source.with_name(source.name + suffix).read_bytes():
                raise ValueError(f"vendored fixture differs from upstream: {vendored}")
        budget_text = source.with_name(source.name + ".budget.expected").read_text().strip()
        match = re.fullmatch(r"\(\{cpu:\s*(\d+)\s*\|\s*mem:\s*(\d+)\}\)", budget_text)
        if match is None or case["expected"]["budget"] != {"cpu": match[1], "mem": match[2]}:
            raise ValueError(f"seed budget differs from upstream: {case['id']}")
        checked += 1
    print(f"verified 350 profile coefficients and {checked} vendored seed input/result/budget triples")
    counts = verify_committed(sources={"plutus": plutus})
    print("verified milestone fixture provenance: " + json.dumps(counts, sort_keys=True))
    counts = verify_builtin_committed(sources={"plutus": plutus})
    print("verified builtin fixture provenance: " + json.dumps(counts, sort_keys=True))


if __name__ == "__main__":
    verify()
