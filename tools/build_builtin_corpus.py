#!/usr/bin/env python3
"""Reconstruct builtin Flat cases from independent raw goldens and explicit ledgers.

Only the two reference encoders and parsers are called. No evaluation response,
from the candidate or a reference, is used to derive an expectation.
"""
import argparse
import copy
import json
import re
from pathlib import Path

from build_milestone_corpus import (
    FLAT_SPEC, MACHINE_SOURCE, OFFICIAL_ROOT, PROFILE, STRICT_DECODER_SOURCE,
    VERSION_SOURCE, attach_flat, attach_normalization, check_source_record,
    encode_jsonl, expected_from_raw, publish, raw_record, reference_records,
    sha256, verify_profile,
)
from conformance import Engine, ROOT, load_cases
from protocol import loads, string, validate_request
from upstreams import verify_source

MANIFEST = "fixtures/builtins/manifest.json"
CORPUS = "fixtures/builtins.jsonl"
DECODER_CORPUS = "fixtures/builtins-decoder.jsonl"
UNSUPPORTED_CORPUS = "fixtures/builtins-unsupported.jsonl"
CANDIDATE_CORPUS = "fixtures/builtins-candidate.jsonl"
AUDIT_CORPUS = "fixtures/builtins-reference-audit.jsonl"
SPEC_SOURCES = (
    MACHINE_SOURCE, FLAT_SPEC, VERSION_SOURCE, STRICT_DECODER_SOURCE,
    "plutus-core/plutus-core/src/PlutusCore/Default/Builtins.hs",
    "plutus-core/plutus-core/src/PlutusCore/Default/Universe/Cardano.hs",
    "plutus-core/plutus-core/src/PlutusCore/Builtin/Meaning.hs",
    "plutus-core/plutus-core/src/PlutusCore/Evaluation/Machine/ExMemoryUsage.hs",
    "plutus-core/plutus-core/src/PlutusCore/Evaluation/Machine/CostingFun/Core.hs",
    "plutus-core/cost-model/data/builtinCostModelE.json",
    "plutus-ledger-api/src/PlutusLedgerApi/V3/ParamName.hs",
)
BUILTINS = {"addInteger": 0, "subtractInteger": 1, "multiplyInteger": 2,
            "equalsInteger": 7, "lessThanInteger": 8,
            "lessThanEqualsInteger": 9, "ifThenElse": 26}
# These positions are ledger API order in pinned V3/ParamName.hs, not a candidate import.
STEP_INDICES = {"constant": 21, "var": 31, "lambda": 27, "apply": 17,
                "delay": 23, "force": 25, "builtin": 19, "startup": 29}
# Enumeration order in pinned Cek/Internal.hs, not ledger-vector order.
FLUSH_ORDER = ("constant", "var", "lambda", "apply", "delay", "force", "builtin")
COST_INDICES = {"addInteger": 0, "subtractInteger": 167, "multiplyInteger": 124,
                "equalsInteger": 71, "lessThanInteger": 99,
                "lessThanEqualsInteger": 96, "ifThenElse": 84}
LIMIT = {"cpu": "1000000000000", "mem": "1000000000000"}
I64_MAX = 2**63 - 1


def integer_memory(value):
    """Pinned ExMemoryUsage: zero=1; floor(log2(abs(n))/64)+1 otherwise."""
    # Corpus decimal strings are bounded to the 64KiB implementation magnitude.
    # Chunked conversion avoids changing Python's process-global decimal limit.
    digits = str(value).removeprefix("-")
    if len(digits) > 157827:
        raise ValueError("integer derivation exceeds the portable magnitude bound")
    number = 0
    for offset in range(0, len(digits), 4000):
        chunk = digits[offset:offset + 4000]
        number = number * 10**len(chunk) + int(chunk)
    if number.bit_length() > 524288:
        raise ValueError("integer derivation exceeds the portable magnitude bound")
    return max(1, (number.bit_length() + 63) // 64)


def builtin_budget(name, arguments, parameters):
    """Independent expressions for the seven selected semantic-E builtins."""
    p = [int(value) for value in parameters]
    i = COST_INDICES[name]
    if name == "ifThenElse":
        cpu, mem = p[i], p[i + 1]
    else:
        if len(arguments) != 2:
            raise ValueError("integer cost expression requires two arguments")
        x, y = map(integer_memory, arguments)
        if name in ("addInteger", "subtractInteger"):
            cpu, mem = p[i] + p[i + 1] * max(x, y), p[i + 2] + p[i + 3] * max(x, y)
        elif name == "multiplyInteger":
            cpu, mem = p[i] + p[i + 1] * x * y, p[i + 2] + p[i + 3] * (x + y)
        else:
            cpu, mem = p[i] + p[i + 1] * min(x, y), p[i + 2]
    return {"cpu": str(cpu), "mem": str(mem)}


def ledger_outcome(events, parameters, limit, term=None):
    """Charge an explicit independently counted execution ledger, not a UPLC AST.

    Each string is a compute visit, halt or error. A dictionary is an already
    type-checked saturated builtin charge. No term parsing/evaluation occurs.
    """
    pending = dict.fromkeys(FLUSH_ORDER, 0)
    consumed = {"cpu": 0, "mem": 0}
    limit = {key: int(value) for key, value in limit.items()}

    def charge(budget):
        for key in consumed:
            amount = int(budget[key])
            if amount < 0:
                raise ValueError("negative computed charge is an unsupported implementation policy")
            consumed[key] += amount
        if any(value > I64_MAX for value in consumed.values()):
            return {"status": "failure", "kind": "budget_exhausted", "budget": None, "traces": []}
        if any(consumed[key] > limit[key] for key in consumed):
            return {"status": "failure", "kind": "budget_exhausted", "budget": dump(), "traces": []}
        return None

    def dump():
        return {key: str(value) for key, value in consumed.items()}

    def step(name, count=1):
        i = STEP_INDICES[name]
        return {"cpu": str(count * int(parameters[i])), "mem": str(count * int(parameters[i + 1]))}

    def flush():
        for name in FLUSH_ORDER:
            if pending[name]:
                failed = charge(step(name, pending[name]))
                if failed:
                    return failed
        pending.update(dict.fromkeys(FLUSH_ORDER, 0))
        return None

    failed = charge(step("startup"))
    if failed:
        return failed
    for index, event in enumerate(events):
        if isinstance(event, str) and event in ("halt", "error"):
            if index != len(events) - 1:
                raise ValueError("terminal event must be last")
            if event == "error":
                return {"status": "failure", "kind": "evaluation", "budget": dump(), "traces": []}
            failed = flush()
            return failed or {"status": "success", "term": term, "budget": dump(), "traces": []}
        if isinstance(event, dict):
            if set(event) != {"builtin", "arguments"} or event["builtin"] not in BUILTINS:
                raise ValueError("invalid builtin ledger event")
            failed = charge(builtin_budget(event["builtin"], event["arguments"], parameters))
        elif event in pending:
            pending[event] += 1
            failed = flush() if sum(pending.values()) == 200 else None
        else:
            raise ValueError("unknown execution ledger event")
        if failed:
            return failed
    raise ValueError("ledger has no terminal event")


def verify_parameter_order(source):
    names = re.findall(r"^  [=|] (\S+)", source, re.M)
    for name, index in COST_INDICES.items():
        if names[index] != name[0].upper() + name[1:] + "'cpu'arguments" + ("" if name == "ifThenElse" else "'intercept"):
            raise ValueError("pinned ledger parameter position differs: " + name)
    for name, index in STEP_INDICES.items():
        stem = {"constant": "Const", "lambda": "Lam"}.get(name, name.title())
        if names[index:index + 2] != [f"Cek{stem}Cost'exBudgetCPU", f"Cek{stem}Cost'exBudgetMemory"]:
            raise ValueError("pinned CEK parameter position differs: " + name)


def custom_profiles(manifest, base):
    result = {}
    for model in manifest["custom_models"]:
        profile = copy.deepcopy(base)
        profile["id"] = "builtin-independent-" + model["id"]
        parameters = ["0"] * 350 if model.get("zero_base") else profile["cost_model"]["parameters"].copy()
        for position, value in model["parameters"].items():
            parameters[int(position)] = value
        profile["cost_model"] = {"parameters": parameters,
                                 "sha256": sha256(("[" + ",".join(parameters) + "]").encode())}
        profile["provenance"] = {"kind": "independent-custom-cost-model", "manifest": MANIFEST,
                                 "derivation": copy.deepcopy(model), "base_profile": PROFILE,
                                 "base_sha256": base["cost_model"]["sha256"]}
        result[f"fixtures/builtins/profiles/{model['id']}.json"] = profile
    return result


def new_case(case_id, source, provenance, profile=PROFILE):
    return {"id": "builtins/" + case_id, "profile": profile,
            "program": {"format": "uplc_text", "source": source},
            "mode": {"kind": "restricting", "budget": LIMIT.copy()}, "provenance": provenance}


def check_request(case, root):
    """Validate the wire envelope using profiles from the root being verified."""
    root = root.resolve()
    string(case.get("profile"), nonempty=True)
    profile_path = (root / case["profile"]).resolve()
    profile_path.relative_to(root)
    profile = loads(profile_path.read_text())
    # As in conformance.make_request, on-disk provenance is not a wire field.
    if type(profile) is dict:
        profile = {key: profile[key] for key in ("id", "language", "protocol_major", "cost_model") if key in profile}
    validate_request({"schema_version": 1, "id": case.get("id"), "program": case.get("program"),
                      "profile": profile, "mode": case.get("mode")})


def expected_probe(probe, profile, budget=None):
    budget = probe.get("budget", LIMIT) if budget is None else budget
    return ledger_outcome(probe["events"], profile["cost_model"]["parameters"], budget,
                          copy.deepcopy(probe.get("term")))


def build_cases(manifest, pins, sources, encoders, normalizers, root=ROOT):
    base = json.loads((root / PROFILE).read_text())
    models = {PROFILE: base, **custom_profiles(manifest, base)}
    common = {"plutus_revision": pins["plutus"]["revision"],
              "spec_sources": {path: sha256((sources["plutus"] / path).read_bytes()) for path in SPEC_SOURCES}}
    cases, candidate, audit = [], [], []
    for relative in manifest["official"]:
        path = Path(OFFICIAL_ROOT) / relative
        raw = {suffix: (sources["plutus"] / (str(path) + suffix)).read_bytes()
               for suffix in ("", ".expected", ".budget.expected")}
        provenance = dict(common, kind="official-plutus-builtin-derived-flat", repository=pins["plutus"]["repository"],
                          source=raw_record(path, raw[""]),
                          goldens={"result": raw_record(str(path) + ".expected", raw[".expected"]),
                                   "budget": raw_record(str(path) + ".budget.expected", raw[".budget.expected"])})
        case = new_case("plutus/" + relative, raw[""].decode(), provenance)
        case["expected"] = expected_from_raw(raw[".expected"].decode(), raw[".budget.expected"].decode())
        attach_flat(case, pins, encoders)
        if case["expected"]["status"] == "success":
            attach_normalization(case, raw[".expected"].decode(), pins, normalizers)
        if relative in manifest["official_reference_audit"]:
            case["provenance"]["reference_disagreement"] = manifest["official_reference_audit"][relative]
            candidate.append(case)
            audit.append(copy.deepcopy(case))
        else:
            cases.append(case)
    old_deferred = {case["id"]: case for case in load_cases([root / "fixtures/milestone-unsupported.jsonl"])}
    for probe in manifest["probes"]:
        profile = probe.get("profile", PROFILE)
        provenance = dict(common, kind="independent-builtin-derivation", manifest=MANIFEST,
                          source=raw_record(MANIFEST + "#" + probe["id"], probe["source"].encode()),
                          derivation=copy.deepcopy(probe))
        if "graduated_from" in probe:
            original = old_deferred[probe["graduated_from"]]
            provenance["graduation"] = {"id": original["id"], "corpus": "fixtures/milestone-unsupported.jsonl",
                                        "case_sha256": sha256(encode_jsonl([original])),
                                        "original_provenance": copy.deepcopy(original["provenance"])}
        case = new_case("probe/" + probe["id"], probe["source"], provenance, profile)
        case["mode"]["budget"] = copy.deepcopy(probe.get("budget", LIMIT))
        case["expected"] = expected_probe(probe, models[profile])
        attach_flat(case, pins, encoders)
        if "expected_source" in probe:
            outcome = copy.deepcopy(case["expected"])
            attach_normalization(case, probe["expected_source"], pins, normalizers)
            if case["expected"]["term"] != probe["term"]:
                raise ValueError("hand-derived expected term disagrees with both parsers: " + probe["id"])
            case["expected"] = outcome
        (candidate if probe.get("candidate_only") else cases).append(case)
        if probe.get("reference_disagreement"):
            audit.append(copy.deepcopy(case))
    by_id = {case["id"]: case for case in cases + candidate}
    probes = {"builtins/probe/" + probe["id"]: probe for probe in manifest["probes"]}
    for parent in manifest["budget_boundaries"]:
        original = by_id["builtins/probe/" + parent]
        required = original["expected"]["budget"]
        for variant in ("exact", "cpu-minus-one", "mem-minus-one", "zero"):
            case = copy.deepcopy(original)
            case["id"] += "/budget-" + variant
            limit = required.copy()
            if variant == "zero":
                limit = {"cpu": "0", "mem": "0"}
            elif variant != "exact":
                component = variant.split("-")[0]
                limit[component] = str(int(limit[component]) - 1)
            case["mode"]["budget"] = limit
            case["expected"] = expected_probe(probes[original["id"]], models[case["profile"]], limit)
            case["provenance"]["budget_boundary"] = {"parent": original["id"], "variant": variant,
                                                        "required_success_budget": required.copy()}
            cases.append(case)
    decoder = []
    for probe in manifest["decoder"]:
        case = new_case("decoder/" + probe["id"], "", dict(common, kind="independent-builtin-decoder-derivation",
                        manifest=MANIFEST, derivation=copy.deepcopy(probe)))
        case["program"] = {"format": "flat", "hex": probe["hex"]}
        case["expected"] = copy.deepcopy(probe["expected"])
        case["provenance"]["flat_sha256"] = sha256(bytes.fromhex(probe["hex"]))
        decoder.append(case)
    graduated = {probe["graduated_from"] for probe in manifest["probes"] if "graduated_from" in probe}
    unsupported = [copy.deepcopy(case) for name, case in old_deferred.items() if name not in graduated]
    for probe in manifest["unsupported"]:
        case = new_case("unsupported/" + probe["id"], probe["source"],
                        dict(common, kind="independent-builtin-unsupported-derivation", manifest=MANIFEST,
                             source=raw_record(MANIFEST + "#" + probe["id"], probe["source"].encode()),
                             derivation=copy.deepcopy(probe)))
        attach_flat(case, pins, encoders)
        unsupported.append(case)
    return {CORPUS: cases, DECODER_CORPUS: decoder, CANDIDATE_CORPUS: candidate,
            AUDIT_CORPUS: audit, UNSUPPORTED_CORPUS: unsupported}


def verify_committed(root=ROOT, sources=None):
    """Reconstruct all decisions using recorded pinned encode/parse results only.

    --check additionally obtains fresh encoder/parser results. Stored responses
    are never trusted for semantics or costs; explicit ledgers/raw goldens remain
    the expectation authority.
    """
    pins = json.loads((root / "upstreams.lock.json").read_text())
    manifest = json.loads((root / MANIFEST).read_text())
    base = json.loads((root / PROFILE).read_text())
    for path, profile in custom_profiles(manifest, base).items():
        if json.loads((root / path).read_text()) != profile:
            raise ValueError("custom profile differs from manifest")
    outputs = {path: load_cases([root / path]) for path in (CORPUS, DECODER_CORPUS, CANDIDATE_CORPUS, AUDIT_CORPUS, UNSUPPORTED_CORPUS)}
    cases = outputs[CORPUS] + outputs[CANDIDATE_CORPUS]
    probes = {probe["id"]: probe for probe in manifest["probes"]}
    expected_ids = {"builtins/plutus/" + path for path in manifest["official"]}
    expected_ids.update("builtins/probe/" + name for name in probes)
    expected_ids.update("builtins/probe/" + parent + "/budget-" + variant
                        for parent in manifest["budget_boundaries"]
                        for variant in ("exact", "cpu-minus-one", "mem-minus-one", "zero"))
    if {case["id"] for case in cases} != expected_ids or len(cases) != len(expected_ids):
        raise ValueError("builtin selection differs from manifest")
    old_deferred = {case["id"]: case for case in load_cases([root / "fixtures/milestone-unsupported.jsonl"])}
    for case in cases:
        p = case["provenance"]
        check_common(case, pins, sources)
        if case["id"].startswith("builtins/plutus/"):
            relative = case["id"].removeprefix("builtins/plutus/")
            if case.get("profile") != PROFILE or case.get("mode") != {"kind": "restricting", "budget": LIMIT}:
                raise ValueError("official fixture profile or budget mode changed")
            path = Path(OFFICIAL_ROOT) / relative
            raw = {}
            for label, suffix in (("source", ""), ("result", ".expected"), ("budget", ".budget.expected")):
                data = (root / "fixtures/builtins/plutus" / (relative + suffix)).read_bytes()
                record = p["source"] if label == "source" else p["goldens"][label]
                if record["path"] != str(path) + suffix:
                    raise ValueError("official raw path changed")
                check_source_record(record, data)
                if sources is not None and data != (sources["plutus"] / (str(path) + suffix)).read_bytes():
                    raise ValueError("official vendored bytes differ from pinned source")
                raw[label] = data.decode()
            is_audit = relative in manifest["official_reference_audit"]
            if (case in outputs[CANDIDATE_CORPUS]) != is_audit:
                raise ValueError("official reference disagreement reclassified")
            if is_audit and p.get("reference_disagreement") != manifest["official_reference_audit"][relative]:
                raise ValueError("official reference disagreement changed")
            expected = expected_from_raw(raw["result"], raw["budget"])
            if expected["status"] == "success":
                expected["term"] = reference_records(p["normalizers"], "normalizer", pins)[0]["outcome"]["term"]
        else:
            name = case["id"].removeprefix("builtins/probe/").split("/budget-")[0]
            probe = probes[name]
            if p["derivation"] != probe or p["manifest"] != MANIFEST:
                raise ValueError("builtin derivation differs from manifest")
            check_source_record(p["source"], probe["source"].encode())
            if case.get("profile") != probe.get("profile", PROFILE):
                raise ValueError("builtin profile differs from derivation")
            if "graduated_from" in probe:
                original = old_deferred[probe["graduated_from"]]
                required = {"id": original["id"], "corpus": "fixtures/milestone-unsupported.jsonl",
                            "case_sha256": sha256(encode_jsonl([original])), "original_provenance": original["provenance"]}
                if p.get("graduation") != required or case["program"] != original["program"]:
                    raise ValueError("graduation changed the prior fixture provenance or Flat bytes")
            profile = json.loads((root / case["profile"]).read_text())
            budget = copy.deepcopy(probe.get("budget", LIMIT))
            if "/budget-" in case["id"]:
                variant = case["id"].split("/budget-")[1]
                required = expected_probe(probe, profile)["budget"]
                boundary = {"parent": "builtins/probe/" + name, "variant": variant,
                            "required_success_budget": required.copy()}
                if p.get("budget_boundary") != boundary:
                    raise ValueError("budget boundary derivation changed")
                budget = required.copy()
                if variant == "zero":
                    budget = {"cpu": "0", "mem": "0"}
                elif variant != "exact":
                    component = variant.split("-")[0]
                    budget[component] = str(int(budget[component]) - 1)
            expected = expected_probe(probe, profile, budget)
            if case.get("mode") != {"kind": "restricting", "budget": budget}:
                raise ValueError("budget boundary limit changed")
            if "expected_source" in probe:
                if reference_records(p["normalizers"], "normalizer", pins)[0]["outcome"]["term"] != probe["term"]:
                    raise ValueError("hand-derived term disagrees with recorded parsers")
            if (case in outputs[CANDIDATE_CORPUS]) != bool(probe.get("candidate_only")):
                raise ValueError("candidate-only probe reclassified")
        if case["expected"] != expected:
            raise ValueError("builtin expected result or budget differs from independent derivation")
    decoder = outputs[DECODER_CORPUS]
    if len(decoder) != len(manifest["decoder"]):
        raise ValueError("decoder selection differs")
    for case, probe in zip(decoder, manifest["decoder"]):
        if (case["id"] != "builtins/decoder/" + probe["id"] or case.get("profile") != PROFILE or
                case.get("mode") != {"kind": "restricting", "budget": LIMIT}):
            raise ValueError("decoder request differs from manifest")
        check_common(case, pins, sources, encoders=False)
        if case["provenance"]["derivation"] != probe or case["program"] != {"format": "flat", "hex": probe["hex"]} or case["expected"] != probe["expected"]:
            raise ValueError("independent decoder fixture changed")
    graduated = {probe["graduated_from"] for probe in manifest["probes"] if "graduated_from" in probe}
    retained = [case for name, case in old_deferred.items() if name not in graduated]
    unsupported = outputs[UNSUPPORTED_CORPUS]
    if unsupported[:len(retained)] != retained or len(unsupported) != len(retained) + len(manifest["unsupported"]):
        raise ValueError("remaining deferred coverage or original provenance changed")
    for case, probe in zip(unsupported[len(retained):], manifest["unsupported"]):
        if (case["id"] != "builtins/unsupported/" + probe["id"] or case.get("profile") != PROFILE or
                case.get("mode") != {"kind": "restricting", "budget": LIMIT}):
            raise ValueError("unsupported request differs from manifest")
        check_common(case, pins, sources)
        if case["provenance"]["derivation"] != probe or "expected" in case:
            raise ValueError("unsupported fixture changed")
        check_source_record(case["provenance"]["source"], probe["source"].encode())
    expected_audit = [case for case in cases if case["provenance"].get("reference_disagreement") or
                      case["provenance"].get("derivation", {}).get("reference_disagreement")]
    if outputs[AUDIT_CORPUS] != expected_audit:
        raise ValueError("reference disagreements must remain visible and unchanged")
    for corpus in outputs.values():
        for case in corpus:
            check_request(case, root)
    return {"builtins": len(outputs[CORPUS]), "decoder": len(decoder),
            "candidate": len(outputs[CANDIDATE_CORPUS]), "audit": len(outputs[AUDIT_CORPUS]), "unsupported": len(unsupported),
            "official": len(manifest["official"]), "graduated": len(graduated)}


def check_common(case, pins, sources, encoders=True):
    p = case["provenance"]
    if p["plutus_revision"] != pins["plutus"]["revision"] or set(p["spec_sources"]) != set(SPEC_SOURCES):
        raise ValueError("builtin specification provenance incomplete")
    for path, digest in p["spec_sources"].items():
        if re.fullmatch(r"[0-9a-f]{64}", digest) is None:
            raise ValueError("invalid specification hash")
        if sources is not None and sha256((sources["plutus"] / path).read_bytes()) != digest:
            raise ValueError("specification hash mismatch")
    if case["program"]["format"] != "flat" or p["flat_sha256"] != sha256(bytes.fromhex(case["program"]["hex"])):
        raise ValueError("builtin Flat hash mismatch")
    if encoders and reference_records(p["encoders"], "flat-encoder", pins)[0]["outcome"]["term"] != ["constant", ["bytes", case["program"]["hex"]]]:
        raise ValueError("Flat bytes disagree with both encoders")


def main(args):
    pins = json.loads((ROOT / "upstreams.lock.json").read_text())
    sources = {name: verify_source(name, pin) for name, pin in pins.items()}
    base = json.loads((ROOT / PROFILE).read_text())
    verify_profile(base, sources["aiken"])
    verify_parameter_order((sources["plutus"] / SPEC_SOURCES[-1]).read_text())
    manifest = json.loads((ROOT / MANIFEST).read_text())
    # The encoding/normalization protocol reads profile files. Publish custom
    # models first, while refusing to replace any pre-existing bytes.
    for path, profile in custom_profiles(manifest, base).items():
        data = (json.dumps(profile, indent=2) + "\n").encode()
        destination = ROOT / path
        if args.check or destination.exists():
            if destination.read_bytes() != data:
                raise ValueError("custom model differs from independent reconstruction")
        else:
            publish(destination, data)
    commands = [args.aiken, args.amaru]
    encoders = [Engine(name + "-flat-encoder", command + " --encode-flat", args.timeout)
                for name, command in zip(("aiken", "amaru"), commands)]
    normalizers = [Engine(name + "-normalizer", command + " --normalize", args.timeout)
                   for name, command in zip(("aiken", "amaru"), commands)]
    try:
        outputs = build_cases(manifest, pins, sources, encoders, normalizers)
    finally:
        for engine in encoders + normalizers:
            engine.close()
    for relative in manifest["official"]:
        for suffix in ("", ".expected", ".budget.expected"):
            data = (sources["plutus"] / OFFICIAL_ROOT / (relative + suffix)).read_bytes()
            destination = ROOT / "fixtures/builtins/plutus" / (relative + suffix)
            if args.check or destination.exists():
                if destination.read_bytes() != data:
                    raise ValueError("official vendored bytes changed")
            else:
                publish(destination, data)
    for path, cases in outputs.items():
        data = encode_jsonl(cases)
        if args.check:
            if (ROOT / path).read_bytes() != data:
                raise ValueError("builtin corpus differs from independent reconstruction: " + path)
        else:
            publish(ROOT / path, data)
    print(json.dumps(dict(verify_committed(sources=sources), checked=args.check)))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--aiken", default="tools/oracle-aiken/target/debug/oracle-aiken")
    parser.add_argument("--amaru", default="tools/oracle-amaru/target/debug/oracle-amaru")
    parser.add_argument("--timeout", type=float, default=10)
    parser.add_argument("--check", action="store_true")
    main(parser.parse_args())
