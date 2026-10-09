# JSONL protocol, version 1

One request and one response per line. Adapters keep stdout reserved for JSON;
diagnostics go to stderr. The runner bounds requests/responses with deadlines and
isolates interpreter crashes in child processes. The request limit is 8 MiB of
UTF-8 JSON body bytes, excluding the native JSONL framing LF; the runner limits
a response to 16 MiB. Wasm exports the equivalent
`evaluate_json(string): string` API.
Response lines must end with a newline. On POSIX the runner kills the process
group on failure, including children that inherit the evaluator's pipes.

The native server also accepts a final request body terminated by EOF. It reads
at most 8 MiB plus one framing/lookahead byte for a request. An overlong body
terminates the native process with an error and no response for that request;
it does not drain the remaining input. The Python runner rejects overlong bodies
before sending them. Wasm receives a complete body and returns an infrastructure
error for an overlong input. Sizes count UTF-8 bytes, including JSON whitespace,
not Unicode characters; only the native framing LF is excluded.

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
Profile IDs are descriptive labels; language, protocol, and the complete hashed
coefficient vector determine behavior. A changed vector with a new valid hash is
an explicit custom model, not a request to use defaults. The fixture importer
requires the pinned fixture model because its budget goldens depend on it.

The candidate accepts only raw Flat, restricting mode,
PlutusV3/protocol 11, and exactly 350 supplied coefficients. Wrong vector length
is an infrastructure error; negative machine-step coefficients are unsupported.
Negative builtin polynomial coefficients are valid and are not substituted.
The builtin subset is `addInteger`, `subtractInteger`, `multiplyInteger`,
`equalsInteger`, `lessThanInteger`, `lessThanEqualsInteger`, and `ifThenElse`.
Partial and forced builtins normalize structurally using existing term syntax.
Textual UPLC, counting, other profiles, other builtins, constr/case, and complex
constants remain unsupported, including in unevaluated lambda/delay bodies.
The candidate does not implement ledger argument construction or CBOR unwrapping.

## Response

Every response contains `schema_version`, the echoed `id`, `engine`, `revision`,
and an `outcome`. Requests that cannot be deserialized (or exceed the size limit)
may return an empty `id`. The outcomes are:

| Status | Fields |
| --- | --- |
| `success` | `term`, `budget: {cpu, mem}`, `traces` |
| `failure` | `kind: decode/evaluation/budget_exhausted`, `budget` or null, `traces`, `diagnostic` |
| `unsupported` | `reason` |
| `infrastructure_error` | `diagnostic` |

The candidate's `revision` is `<package-version>+git.<full-build-commit>`;
`.dirty` is appended when the build sees staged, unstaged, or nonignored
untracked files anywhere in the repository. Ignored build outputs do not mark
it dirty. Builds without usable Git metadata report
`<package-version>+git.unknown`, including source archives inside unrelated
repositories. Native and Wasm use the same core identity. This is a build-time
snapshot; it does not describe later edits or certify reproducible builds.
The small identity build script intentionally reruns on each Cargo build so
incremental builds notice HEAD, dirty-state, and Git-metadata changes, including
linked worktrees. Reference adapters retain their pinned source commit IDs.
The runner rejects unknown response fields, duplicate JSON fields, nonfinite JSON
numbers, malformed normalized constructors, and missing outcome fields. Every
integer in a normalized term must be a canonical decimal string; booleans must
be JSON booleans. Consumed budgets must be nonnegative i64 strings.

Malformed Flat fails decoding before startup and
has a null budget. Startup is charged immediately; machine compute events use
the official 200-event batching policy and flush on successful termination.
Semantic errors do not flush pending events. A saturated builtin validates its
argument types and semantics-E integer input bounds, then charges immediately
without flushing pending CEK events, then debits portable implementation work
and executes. Missing/excess forces, application before required forcing, and
unlifting failures have no builtin application charge. Negative coefficients
are allowed; a negative computed CPU or memory charge is unsupported, without
clamping or crediting the budget. Exhaustion includes the entire
attempted CPU and memory charge. Checked overflow beyond the i64 wire range is
`budget_exhausted` with a null budget, never wrapping or saturation. Implementation
resource/result limits yield unsupported. See [the milestone policy](milestone.md)
for charging order, supported decoding guarantees, and portable bounds.

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
Imported cases retain raw upstream result/budget texts and hashes, plus the
identity and outcome of each golden parser, in `provenance`. A normalized term
is used only when two independent parsers agree. Parser failures or disagreements
leave the term unavailable while retaining independent status/cost expectations.

Every pair of engines is compared. Priority is infrastructure error, mismatch,
unsupported, then pass. `--allow-unsupported` changes the exit policy only; the
report still has `complete: false` and unsupported cases never count as passes.
The same policy applies when both engines report unsupported or both crash.
