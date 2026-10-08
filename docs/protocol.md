# JSONL protocol, version 1

One request and one response per line. Adapters keep stdout reserved for JSON;
diagnostics go to stderr. The runner bounds requests/responses with deadlines and
isolates interpreter crashes in child processes. The Rust server limits a request
to 8 MiB and the runner limits a response to 16 MiB. Wasm exports the equivalent
`evaluate_json(string): string` API.

## Request

```json
{
  "schema_version": 1,
  "id": "unique-case-id",
  "program": {"format": "uplc_text", "source": "(program 1.0.0 (con integer 42))"},
  "profile": {
    "id": "profile-name",
    "language": "PlutusV3",
    "protocol_major": 11,
    "cost_model": {"parameters": ["..."], "sha256": "..."}
  },
  "mode": {"kind": "restricting", "budget": {"cpu": "10000000", "mem": "1000000"}}
}
```

The placeholders above must be replaced by the complete profile parameter
vector and its hash. `make_request` in `tools/conformance.py` loads these from
the case's profile file. The other program encoding is
`{"format":"flat","hex":"010000200101"}`: raw Flat bytes, with no CBOR wrapper.
Programs have their arguments already applied; there is no implicit application
or script-context generation. The UPLC version is encoded inside the program.

The other mode is `{"kind":"counting"}`. Engines that do not implement genuine
counting return unsupported. No engine may silently replace an unknown profile,
parameter vector, or mode with its defaults.

Parameters and execution units are canonical decimal strings. The SHA-256
preimage for coefficients `["1","-2","3"]` is the UTF-8 byte string `[1,-2,3]`.
Parameter order is the ledger API order for the selected profile. Unknown fields,
noncanonical decimals, out-of-range i64 coefficients/limits, and wrong hashes
are request errors, not semantic UPLC failures.

## Response

Every response contains `schema_version`, the echoed `id`, `engine`, `revision`,
and an `outcome`. The outcomes are:

| Status | Fields |
| --- | --- |
| `success` | `term`, `budget: {cpu, mem}`, `traces` |
| `failure` | `kind: decode/evaluation/budget_exhausted`, `budget` or null, `traces`, `diagnostic` |
| `unsupported` | `reason` |
| `infrastructure_error` | `diagnostic` |

The candidate currently identifies its revision by package version. Reference
adapters use pinned source commit IDs. Add the candidate build commit to the
identity before using reports as release provenance.

## Normalized terms

Terms are JSON arrays, independent of either Rust AST:

```text
["var", "1"]                         -- one-based De Bruijn index
["lambda", body]                       -- binder names deliberately omitted
["apply", function, argument]
["delay", term] / ["force", term]
["constant", constant]
["builtin", "0"]                      -- builtin tag, decimal string
["error"]
["constr", "0", [field, ...]]
["case", scrutinee, [branch, ...]]
```

Primitive constants are `["integer","42"]`, `["bytes","00ff"]`,
`["string","hello"]`, `["bool",true]`, and `["unit"]`. Composite constants are
`["list", element_type, [constant,...]]` and
`["pair", first_type, second_type, first, second]`. Types use primitive tag
strings, `["list",type]`, or `["pair",type,type]`.

Future Data normalization must preserve map order and duplicate entries. BLS
normalization must use defined encodings or observable equality, not debug
formatting. Reference result normalizers stop at depth 512 and explicitly mark
unhandled values unsupported. Do not reduce under lambdas or run an optimizer
as part of normalization.

## Case files and comparisons

Corpus files contain JSONL objects with `id`, `profile` (a repository-relative
path), `program`, `mode`, optional `expected`, and `provenance`. Expected outputs
can omit information the upstream golden does not provide, such as traces or
failure costs. At least two engines or a golden are needed for a comparison.
An `unavailable` field records an importer limitation and prevents a pass.

Every pair of engines is compared. Priority is infrastructure error, mismatch,
unsupported, then pass. `--allow-unsupported` changes the exit policy only; the
report still has `complete: false` and unsupported cases never count as passes.
The same policy applies when both engines report unsupported or both crash.
