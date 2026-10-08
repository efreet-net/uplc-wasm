#!/usr/bin/env python3
"""Build Flat fixtures using two pinned encoders and independent semantic goldens.

This tool only calls --encode-flat and --normalize. It never evaluates a
candidate or derives an expected result/cost from an evaluator response.
"""
import argparse
import copy
import hashlib
import json
import os
from pathlib import Path
import re
import tempfile

from conformance import Engine, ROOT, load_cases, make_request
from protocol import validate_outcome
from upstreams import verify_source

PROFILE = "profiles/plutus-v3-pv11.json"
MANIFEST = "fixtures/milestone/manifest.json"
CORPUS = "fixtures/milestone.jsonl"
DECODER_CORPUS = "fixtures/milestone-decoder.jsonl"
UNSUPPORTED_CORPUS = "fixtures/milestone-unsupported.jsonl"
AUDIT_CORPUS = "fixtures/milestone-failure-policy-audit.jsonl"
EXTRA_CORPORA = {"decoder": DECODER_CORPUS, "unsupported": UNSUPPORTED_CORPUS, "audit": AUDIT_CORPUS}
OFFICIAL_ROOT = "plutus-conformance/test-cases/uplc/evaluation"
MACHINE_SOURCE = "plutus-core/untyped-plutus-core/src/UntypedPlutusCore/Evaluation/Machine/Cek/Internal.hs"
FLAT_SPEC = "doc/plutus-core-spec/flat-serialisation.tex"
VERSION_SOURCE = "plutus-ledger-api/src/PlutusLedgerApi/Common/Versions.hs"
STRICT_DECODER_SOURCE = "plutus-core/flat/src/PlutusCore/Flat/Decoder/Run.hs"
# Explicit ledger-vector positions, independent of the candidate cost module.
STEP_INDICES = {"apply": 17, "constant": 21, "delay": 23, "force": 25,
                "lambda": 27, "startup": 29, "var": 31}
LIMIT = {"cpu": "1000000000", "mem": "1000000000"}


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def raw_record(path, data):
    return {"path": str(path), "sha256": sha256(data), "text": data.decode("utf-8")}


def expected_from_raw(result, budget):
    result, budget = result.strip(), budget.strip()
    if result in ("evaluation failure", "parse error", "parse/decode error"):
        if budget != result:
            raise ValueError("inconsistent official failure and budget goldens")
        return {"status": "failure", "kind": "evaluation" if result == "evaluation failure" else "decode"}
    match = re.fullmatch(r"\(\{cpu:\s*(\d+)\s*\|\s*mem:\s*(\d+)\}\)", budget)
    if match is None:
        raise ValueError("unrecognized official budget golden")
    return {"status": "success", "budget": {"cpu": match[1], "mem": match[2]}}


def step_budget(steps, profile):
    if steps.get("startup") != 1 or set(steps) - STEP_INDICES.keys():
        raise ValueError("a CEK expectation needs one startup and known machine-step counts")
    parameters = profile["cost_model"]["parameters"]
    if len(parameters) != 350:
        raise ValueError("expected explicit 350-parameter profile")
    cpu = mem = 0
    for name, count in steps.items():
        if type(count) is not int or count < 0:
            raise ValueError("step counts must be nonnegative integers")
        index = STEP_INDICES[name]
        cpu += count * int(parameters[index])
        mem += count * int(parameters[index + 1])
    return {"cpu": str(cpu), "mem": str(mem)}


def verify_profile(profile, aiken):
    source = (aiken / "crates/uplc/tests/conformance.rs").read_text()
    costs = re.search(r"const V3_PV11_COSTS: &\[i64\] = &\[(.*?)\];", source, re.S)
    if costs is None or re.findall(r"-?\d+", costs[1]) != profile["cost_model"]["parameters"]:
        raise ValueError("milestone cost goldens require the exact pinned V3_PV11_COSTS model")
    if (profile["language"], profile["protocol_major"]) != ("PlutusV3", 11):
        raise ValueError("milestone profile must be PlutusV3 / protocol 11")


def reference_records(responses, purpose, pins):
    """Require actual pinned identities, not two aliases for one parser/candidate."""
    expected = {f"{name}-{purpose}": pin["revision"] for name, pin in pins.items()
                if name in ("aiken", "amaru")}
    identities = {response["engine"]: response["revision"] for response in responses}
    if len(responses) != 2 or identities != expected:
        raise ValueError(f"expected both pinned {purpose} implementations, got {identities}")
    records = [{key: response[key] for key in ("engine", "revision", "outcome")}
               for response in responses]
    for record in records:
        validate_outcome(record["outcome"])
        if record["outcome"]["status"] != "success":
            raise ValueError(f"{purpose} unavailable: " + json.dumps(records))
        if record["outcome"]["budget"] != {"cpu": "0", "mem": "0"} or record["outcome"]["traces"]:
            raise ValueError(f"{purpose} must parse/encode without evaluation")
    if records[0]["outcome"]["term"] != records[1]["outcome"]["term"]:
        raise ValueError(f"{purpose} disagreement: " + json.dumps(records))
    return records


def check_source_record(record, data):
    if record["sha256"] != sha256(data) or record["text"].encode("utf-8") != data:
        raise ValueError(f"raw provenance mismatch: {record['path']}")


def check_common(case, pins, sources):
    if case["profile"] != PROFILE:
        raise ValueError("milestone fixtures must use the pinned profile")
    p = case["provenance"]
    if p["plutus_revision"] != pins["plutus"]["revision"]:
        raise ValueError("incorrect official Plutus revision")
    if set(p["spec_sources"]) != {MACHINE_SOURCE, FLAT_SPEC, VERSION_SOURCE, STRICT_DECODER_SOURCE}:
        raise ValueError("specification source provenance is incomplete")
    for path, digest in p["spec_sources"].items():
        if re.fullmatch(r"[0-9a-f]{64}", digest) is None:
            raise ValueError("invalid specification source hash")
        if sources is not None and sha256((sources["plutus"] / path).read_bytes()) != digest:
            raise ValueError("specification source hash mismatch")


def new_case(case_id, source, provenance):
    return {"id": "milestone/" + case_id, "profile": PROFILE,
            "program": {"format": "uplc_text", "source": source},
            "mode": {"kind": "restricting", "budget": LIMIT.copy()},
            "provenance": provenance}


def build_cases(manifest, pins, sources, encoders, normalizers, root=ROOT):
    profile = json.loads((root / PROFILE).read_text())
    cases = []
    spec_sources = {path: sha256((sources["plutus"] / path).read_bytes())
                    for path in (MACHINE_SOURCE, FLAT_SPEC, VERSION_SOURCE, STRICT_DECODER_SOURCE)}
    common = {"plutus_revision": pins["plutus"]["revision"], "spec_sources": spec_sources}
    for relative in manifest["official"]:
        path = Path(OFFICIAL_ROOT) / relative
        raw = {suffix: (sources["plutus"] / (str(path) + suffix)).read_bytes()
               for suffix in ("", ".expected", ".budget.expected")}
        provenance = dict(common, kind="official-plutus-derived-flat",
                          repository=pins["plutus"]["repository"],
                          source=raw_record(path, raw[""]),
                          goldens={"result": raw_record(str(path) + ".expected", raw[".expected"]),
                                   "budget": raw_record(str(path) + ".budget.expected", raw[".budget.expected"])})
        case = new_case("plutus/" + relative, raw[""].decode("utf-8"), provenance)
        case["expected"] = expected_from_raw(raw[".expected"].decode("utf-8"), raw[".budget.expected"].decode("utf-8"))
        attach_flat(case, pins, encoders)
        if case["expected"]["status"] == "success":
            attach_normalization(case, raw[".expected"].decode("utf-8"), pins, normalizers)
        cases.append(case)
    for probe in manifest["probes"]:
        provenance = dict(common, kind="independent-cek-derivation", manifest=MANIFEST,
                          source=raw_record(MANIFEST + "#" + probe["id"], probe["source"].encode()),
                          derivation=copy.deepcopy(probe))
        case = new_case("probe/" + probe["id"], probe["source"], provenance)
        case["expected"] = copy.deepcopy(probe["expected"])
        if case["expected"]["status"] == "success":
            case["expected"]["budget"] = step_budget(probe["steps"], profile)
        attach_flat(case, pins, encoders)
        if case["expected"]["status"] == "success":
            independently_expected = copy.deepcopy(case["expected"]["term"])
            attach_normalization(case, probe["expected_source"], pins, normalizers)
            if case["expected"]["term"] != independently_expected:
                raise ValueError("hand-written semantic expectation disagrees with golden parsers: " + probe["id"])
        cases.append(case)
    by_id = {case["id"]: case for case in cases}
    for parent in manifest["budget_boundaries"]:
        original = by_id["milestone/" + parent]
        required = original["expected"]["budget"]
        for variant in ("exact", "cpu-minus-one", "mem-minus-one", "zero"):
            case = copy.deepcopy(original)
            case["id"] += "/budget-" + variant
            case["mode"]["budget"] = required.copy()
            if variant != "exact":
                case["expected"] = {"status": "failure", "kind": "budget_exhausted"}
                if variant == "zero":
                    case["mode"]["budget"] = {"cpu": "0", "mem": "0"}
                else:
                    component = variant.split("-")[0]
                    case["mode"]["budget"][component] = str(int(required[component]) - 1)
            case["provenance"]["budget_boundary"] = {"parent": original["id"], "variant": variant,
                                                       "required_success_budget": required.copy()}
            cases.append(case)
    extras = {}
    for group in EXTRA_CORPORA:
        extras[group] = []
        for probe in manifest[group]:
            case = new_case(group + "/" + probe["id"], probe.get("source", ""),
                            dict(common, kind="independent-" + group + "-derivation",
                                 manifest=MANIFEST, derivation=copy.deepcopy(probe)))
            if "source" in probe:
                case["provenance"]["source"] = raw_record(MANIFEST + "#" + probe["id"], probe["source"].encode())
                attach_flat(case, pins, encoders)
            else:
                case["program"] = {"format": "flat", "hex": probe["hex"]}
                case["provenance"]["flat_sha256"] = sha256(bytes.fromhex(probe["hex"]))
            if "expected" in probe:
                case["expected"] = copy.deepcopy(probe["expected"])
            if "mode" in probe:
                case["mode"] = copy.deepcopy(probe["mode"])
            extras[group].append(case)
    return cases, extras


def attach_flat(case, pins, encoders):
    request = make_request(case)
    records = reference_records([encoder.evaluate(request) for encoder in encoders], "flat-encoder", pins)
    term = records[0]["outcome"]["term"]
    if term[:1] != ["constant"] or term[1][:1] != ["bytes"]:
        raise ValueError("encoder did not return the Flat bytes envelope")
    flat = term[1][1]
    case["program"] = {"format": "flat", "hex": flat}
    case["provenance"]["encoders"] = records
    case["provenance"]["flat_sha256"] = sha256(bytes.fromhex(flat))


def attach_normalization(case, source, pins, normalizers):
    request = make_request(case)
    request["program"] = {"format": "uplc_text", "source": source}
    records = reference_records([normalizer.evaluate(request) for normalizer in normalizers], "normalizer", pins)
    case["provenance"]["normalizers"] = records
    case["expected"]["term"] = records[0]["outcome"]["term"]


def verify_committed(root=ROOT, sources=None):
    """Validate stored derivations and hashes; --check additionally reruns parsers/encoders."""
    pins = json.loads((root / "upstreams.lock.json").read_text())
    manifest = json.loads((root / MANIFEST).read_text())
    profile = json.loads((root / PROFILE).read_text())
    cases = load_cases([root / CORPUS])
    by_id = {case["id"]: case for case in cases}
    probes = {probe["id"]: probe for probe in manifest["probes"]}
    official = set(manifest["official"])
    boundaries = set()
    for parent in manifest["budget_boundaries"]:
        boundaries.update("milestone/" + parent + "/budget-" + variant
                          for variant in ("exact", "cpu-minus-one", "mem-minus-one", "zero"))
    expected_ids = {"milestone/plutus/" + relative for relative in official}
    expected_ids.update("milestone/probe/" + name for name in probes)
    if set(by_id) != expected_ids | boundaries:
        raise ValueError("milestone case selection differs from the explicit manifest")
    for case in cases:
        make_request(case)
        check_common(case, pins, sources)
        p = case["provenance"]
        flat = bytes.fromhex(case["program"]["hex"])
        if case["program"]["format"] != "flat" or sha256(flat) != p["flat_sha256"]:
            raise ValueError("Flat hash mismatch")
        records = reference_records(p["encoders"], "flat-encoder", pins)
        if records[0]["outcome"]["term"] != ["constant", ["bytes", flat.hex()]]:
            raise ValueError("recorded encoders disagree with Flat bytes")
        if p["kind"] == "official-plutus-derived-flat":
            if p["repository"] != pins["plutus"]["repository"]:
                raise ValueError("incorrect official Plutus repository")
            relative = Path(p["source"]["path"]).relative_to(OFFICIAL_ROOT).as_posix()
            if relative not in official or not case["id"].startswith("milestone/plutus/" + relative):
                raise ValueError("official fixture selection mismatch")
            for record, suffix in ((p["source"], ""), (p["goldens"]["result"], ".expected"),
                                   (p["goldens"]["budget"], ".budget.expected")):
                path = str(Path(OFFICIAL_ROOT) / relative) + suffix
                if record["path"] != path:
                    raise ValueError("official fixture path mismatch")
                data = (root / "fixtures/milestone/plutus" / (relative + suffix)).read_bytes()
                check_source_record(record, data)
                if sources is not None and data != (sources["plutus"] / path).read_bytes():
                    raise ValueError("vendored milestone golden differs from pinned upstream")
            expected = expected_from_raw(p["goldens"]["result"]["text"], p["goldens"]["budget"]["text"])
        elif p["kind"] == "independent-cek-derivation":
            probe = probes[p["derivation"]["id"]]
            if p["derivation"] != probe:
                raise ValueError("CEK derivation differs from manifest")
            check_source_record(p["source"], probe["source"].encode())
            expected = copy.deepcopy(probe["expected"])
            if expected["status"] == "success":
                expected["budget"] = step_budget(probe["steps"], profile)
        else:
            raise ValueError("unknown fixture provenance kind")
        if expected["status"] == "success":
            normalized = reference_records(p["normalizers"], "normalizer", pins)[0]["outcome"]["term"]
            if "term" in expected and expected["term"] != normalized:
                raise ValueError("independent expectation differs from golden normalization")
            expected["term"] = normalized
        if "budget_boundary" in p:
            boundary = p["budget_boundary"]
            if case["id"] != boundary["parent"] + "/budget-" + boundary["variant"] or case["id"] not in boundaries:
                raise ValueError("unexpected budget boundary")
            required = by_id[boundary["parent"]]["expected"]["budget"]
            if boundary["required_success_budget"] != required:
                raise ValueError("budget boundary changed required cost")
            limit = required.copy()
            if boundary["variant"] != "exact":
                expected = {"status": "failure", "kind": "budget_exhausted"}
                if boundary["variant"] == "zero":
                    limit = {"cpu": "0", "mem": "0"}
                else:
                    component = boundary["variant"].split("-")[0]
                    limit[component] = str(int(required[component]) - 1)
            if case["mode"]["budget"] != limit:
                raise ValueError("budget boundary limit mismatch")
        elif case["mode"] != {"kind": "restricting", "budget": LIMIT}:
            raise ValueError("unexpected fixture mode or budget")
        if case["expected"] != expected:
            raise ValueError("expected result or budget differs from independent provenance")
    counts = {"milestone": len(cases)}
    for group, path in EXTRA_CORPORA.items():
        extra_cases = load_cases([root / path])
        counts[group] = len(extra_cases)
        if [case["id"] for case in extra_cases] != ["milestone/" + group + "/" + probe["id"] for probe in manifest[group]]:
            raise ValueError(group + " selection differs from manifest")
        for case, probe in zip(extra_cases, manifest[group]):
            make_request(case)
            check_common(case, pins, sources)
            p = case["provenance"]
            if p["kind"] != "independent-" + group + "-derivation" or p["derivation"] != probe:
                raise ValueError(group + " derivation differs from manifest")
            if "source" in probe:
                check_source_record(p["source"], probe["source"].encode())
                encoded = reference_records(p["encoders"], "flat-encoder", pins)[0]["outcome"]["term"]
                if encoded != ["constant", ["bytes", case["program"]["hex"]]]:
                    raise ValueError("encoder bytes mismatch")
            elif case["program"] != {"format": "flat", "hex": probe["hex"]}:
                raise ValueError("raw Flat differs from independent derivation")
            if case.get("expected") != probe.get("expected") or case["mode"] != probe.get("mode", {"kind": "restricting", "budget": LIMIT}):
                raise ValueError(group + " fixture differs from independent expectation")
            if p["flat_sha256"] != sha256(bytes.fromhex(case["program"]["hex"])) or p["plutus_revision"] != pins["plutus"]["revision"]:
                raise ValueError(group + " provenance hash/revision mismatch")
    return counts


def encode_jsonl(cases):
    return "".join(json.dumps(case, ensure_ascii=False, separators=(",", ":")) + "\n" for case in cases).encode()


def publish(path, data):
    """Refuse overwrites, including races; publish complete files atomically."""
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = None
    try:
        with tempfile.NamedTemporaryFile(dir=path.parent, delete=False) as output:
            temporary = Path(output.name)
            output.write(data)
        os.link(temporary, path)
    finally:
        if temporary is not None:
            temporary.unlink()


def main(args):
    pins = json.loads((ROOT / "upstreams.lock.json").read_text())
    sources = {name: verify_source(name, pin) for name, pin in pins.items()}
    verify_profile(json.loads((ROOT / PROFILE).read_text()), sources["aiken"])
    manifest = json.loads((ROOT / MANIFEST).read_text())
    commands = [args.aiken, args.amaru]
    encoders = [Engine(name, command + " --encode-flat", args.timeout)
                for name, command in zip(("aiken-flat-encoder", "amaru-flat-encoder"), commands)]
    normalizers = [Engine(name, command + " --normalize", args.timeout)
                   for name, command in zip(("aiken-normalizer", "amaru-normalizer"), commands)]
    try:
        cases, extras = build_cases(manifest, pins, sources, encoders, normalizers)
    finally:
        for engine in encoders + normalizers:
            engine.close()
    # No files are written if either encoder/parser disagrees or fails.
    for relative in manifest["official"]:
        for suffix in ("", ".expected", ".budget.expected"):
            source = sources["plutus"] / OFFICIAL_ROOT / (relative + suffix)
            destination = ROOT / "fixtures/milestone/plutus" / (relative + suffix)
            if args.check or destination.exists():
                if destination.read_bytes() != source.read_bytes():
                    raise ValueError("vendored official bytes differ: " + str(destination))
            else:
                publish(destination, source.read_bytes())
    outputs = [(ROOT / CORPUS, encode_jsonl(cases))]
    outputs.extend((ROOT / path, encode_jsonl(extras[group])) for group, path in EXTRA_CORPORA.items())
    for path, content in outputs:
        if args.check:
            if path.read_bytes() != content:
                raise ValueError("committed corpus differs from independent reconstruction: " + str(path))
        else:
            publish(path, content)
    counts = verify_committed(sources=sources)
    print(json.dumps(dict(counts, checked=args.check,
                          note="encoders and golden parsers only; no evaluation expectations taken from candidate")))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--aiken", default="tools/oracle-aiken/target/debug/oracle-aiken")
    parser.add_argument("--amaru", default="tools/oracle-amaru/target/debug/oracle-amaru")
    parser.add_argument("--timeout", type=float, default=10)
    parser.add_argument("--check", action="store_true", help="reconstruct and compare without writing")
    main(parser.parse_args())
