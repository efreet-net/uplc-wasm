"""Strict validation of the JSON comparison boundary (no evaluator semantics)."""
import json
import re

MAX_REQUEST = 8 * 1024 * 1024
I64_MIN, I64_MAX = -(2**63), 2**63 - 1


def loads(text):
    def object_pairs(pairs):
        result = {}
        for key, value in pairs:
            if key in result:
                raise ValueError(f"duplicate JSON field: {key}")
            result[key] = value
        return result

    def invalid_constant(value):
        raise ValueError(f"invalid JSON constant: {value}")

    return json.loads(text, object_pairs_hook=object_pairs, parse_constant=invalid_constant)


def fields(value, required, optional=()):
    if type(value) is not dict or not set(required) <= value.keys() or value.keys() - set(required) - set(optional):
        raise ValueError(f"expected fields {', '.join(required)} (optional: {', '.join(optional)})")


def string(value, *, nonempty=False):
    if type(value) is not str or (nonempty and not value):
        raise ValueError("expected a nonempty string" if nonempty else "expected a string")
    value.encode("utf-8")  # Reject unpaired JSON surrogate escapes.


def decimal(value, *, minimum=None, maximum=None):
    # Do not convert arbitrary UPLC integers to Python int: its decimal digit
    # limit is smaller than valid PV11 integers. Bounds are only used for i64s.
    if type(value) is not str or not re.fullmatch(r"0|-?[1-9][0-9]*", value):
        raise ValueError("expected a canonical decimal string")
    if minimum is not None or maximum is not None:
        if len(value) > 20:
            raise ValueError("decimal is out of range")
        number = int(value)
        if (minimum is not None and number < minimum) or (maximum is not None and number > maximum):
            raise ValueError("decimal is out of range")


def budget(value):
    fields(value, ("cpu", "mem"))
    for coefficient in value.values():
        decimal(coefficient, minimum=0, maximum=I64_MAX)


def validate_request(request):
    fields(request, ("schema_version", "id", "program", "profile", "mode"))
    if type(request["schema_version"]) is not int or request["schema_version"] != 1:
        raise ValueError("unsupported request schema")
    string(request["id"], nonempty=True)
    profile = request["profile"]
    fields(profile, ("id", "language", "protocol_major", "cost_model"))
    string(profile["id"], nonempty=True)
    if profile["language"] not in ("PlutusV1", "PlutusV2", "PlutusV3"):
        raise ValueError("unknown language")
    if type(profile["protocol_major"]) is not int or not 0 <= profile["protocol_major"] <= 65535:
        raise ValueError("protocol_major must be a u16")
    model = profile["cost_model"]
    fields(model, ("parameters", "sha256"))
    parameters = model["parameters"]
    if type(parameters) is not list or not 1 <= len(parameters) <= 4096:
        raise ValueError("expected 1..=4096 cost coefficients")
    for value in parameters:
        decimal(value, minimum=I64_MIN, maximum=I64_MAX)
    import hashlib
    digest = hashlib.sha256(("[" + ",".join(parameters) + "]").encode()).hexdigest()
    if model["sha256"] != digest:
        raise ValueError("cost-model hash mismatch")
    program = request["program"]
    fields(program, ("format",), ("source", "hex"))
    if program["format"] == "uplc_text":
        fields(program, ("format", "source"))
        string(program["source"])
    elif program["format"] == "flat":
        fields(program, ("format", "hex"))
        string(program["hex"])
        if not re.fullmatch(r"(?:[0-9a-fA-F]{2})*", program["hex"]):
            raise ValueError("Flat transport must contain hexadecimal bytes")
    else:
        raise ValueError("unknown program format")
    mode = request["mode"]
    fields(mode, ("kind",), ("budget",))
    if mode["kind"] == "restricting":
        fields(mode, ("kind", "budget"))
        budget(mode["budget"])
    elif mode["kind"] == "counting":
        fields(mode, ("kind",))
    else:
        raise ValueError("unknown evaluation mode")


def validate_term(term):
    """Check all currently defined constructors, including nested constants/types."""
    pending = [("term", term, 0, None)]
    primitives = ("integer", "bytes", "string", "bool", "unit")
    while pending:
        kind, value, depth, declared_type = pending.pop()
        if depth > 512:
            raise ValueError("normalized structure exceeds depth 512")
        if kind == "type" and type(value) is str and value in primitives:
            continue
        if type(value) is not list or not value or type(value[0]) is not str:
            raise ValueError("expected a normalized structural array")
        tag = value[0]
        arities = {"term": {"var": 2, "lambda": 2, "apply": 3, "delay": 2, "force": 2,
                            "constant": 2, "builtin": 2, "error": 1, "constr": 3, "case": 3},
                   "constant": {"integer": 2, "bytes": 2, "string": 2, "bool": 2, "unit": 1,
                                "list": 3, "pair": 5}, "type": {"list": 2, "pair": 3}}
        if arities[kind].get(tag) != len(value):
            raise ValueError(f"invalid {kind} constructor or arity: {tag}")
        if kind == "constant" and declared_type is not None:
            actual_type = ["list", value[1]] if tag == "list" else ["pair", *value[1:3]] if tag == "pair" else tag
            if actual_type != declared_type:
                raise ValueError("constant does not match its declared list/pair type")

        def child(child_kind, child_value, expected_type=None):
            pending.append((child_kind, child_value, depth + 1, expected_type))

        def children(child_kind, values, expected_type=None):
            if type(values) is not list:
                raise ValueError("expected an array of children")
            for item in values:
                child(child_kind, item, expected_type)

        if kind == "type":
            children("type", value[1:])
        elif kind == "term":
            if tag in ("var", "builtin", "constr"):
                decimal(value[1])
                if value[1].startswith("-") or (tag == "var" and value[1] == "0"):
                    raise ValueError("invalid unsigned term index/tag")
            if tag in ("lambda", "delay", "force", "apply"):
                children("term", value[1:])
            elif tag == "constant":
                child("constant", value[1])
            elif tag == "constr":
                children("term", value[2])
            elif tag == "case":
                child("term", value[1])
                children("term", value[2])
        elif tag == "integer":
            decimal(value[1])
        elif tag == "bytes":
            string(value[1])
            if not re.fullmatch(r"(?:[0-9a-f]{2})*", value[1]):
                raise ValueError("normalized bytes must be lowercase hex")
        elif tag == "string":
            string(value[1])
        elif tag == "bool":
            if type(value[1]) is not bool:
                raise ValueError("normalized bool must be a JSON boolean")
        elif tag == "list":
            child("type", value[1])
            children("constant", value[2], value[1])
        elif tag == "pair":
            children("type", value[1:3])
            child("constant", value[3], value[1])
            child("constant", value[4], value[2])


def validate_outcome(outcome, *, partial=False):
    fields(outcome, ("status",), ("term", "budget", "traces", "kind", "diagnostic", "reason"))
    status = outcome["status"]
    if status == "success":
        required, optional = ("status", "term", "budget", "traces"), ()
    elif status == "failure":
        required, optional = ("status", "kind", "budget", "traces", "diagnostic"), ()
    elif status in ("unsupported", "infrastructure_error") and not partial:
        required, optional = ("status", "reason" if status == "unsupported" else "diagnostic"), ()
    else:
        raise ValueError("unknown outcome status")
    if partial:
        optional = required[1:]
        required = ("status", "kind") if status == "failure" else ("status",)
    fields(outcome, required, optional)
    if "term" in outcome:
        validate_term(outcome["term"])
    if "kind" in outcome and outcome["kind"] not in ("decode", "evaluation", "budget_exhausted"):
        raise ValueError("unknown failure kind")
    if "budget" in outcome and (outcome["budget"] is not None or status == "success"):
        budget(outcome["budget"])
    if "traces" in outcome:
        if type(outcome["traces"]) is not list:
            raise ValueError("traces must be an array")
        for trace in outcome["traces"]:
            string(trace)
    for field in ("reason", "diagnostic"):
        if field in outcome:
            string(outcome[field])


def validate_response(response, request_id):
    fields(response, ("schema_version", "id", "engine", "revision", "outcome"))
    if type(response["schema_version"]) is not int or response["schema_version"] != 1 or response["id"] != request_id:
        raise ValueError("engine returned the wrong schema or request ID")
    string(response["engine"], nonempty=True)
    string(response["revision"], nonempty=True)
    validate_outcome(response["outcome"])
