# First evaluator milestone

The core independently decodes and evaluates raw Flat UPLC 1.0.0 and 1.1.0 under
PlutusV3/protocol 11. The supported terms are one-based De Bruijn variables,
lambda/application, delay/force, explicit error, and primitive integer,
bytestring, UTF-8 string, boolean, and unit constants. Integers use arbitrary
precision; normalized integers and budgets cross JSON and JavaScript as decimal
strings. The native server and release Wasm export call the same core.

The CEK machine evaluates the function before the argument, evaluates arguments
strictly, captures lexical environments in lambdas and delays, and substitutes
captures when returning structural syntax. It preserves bound indices and does
not reduce lambda or delay bodies during normalization. Open positive variables,
explicit error, applying a non-function, and forcing a non-delay are evaluation
failures. De Bruijn index zero violates the raw Flat variable encoding rule and
fails decoding.

This milestone does not evaluate builtins or constr/case, decode complex
constant payloads, parse textual UPLC, implement genuine counting, historical
profiles, ledger argument construction, or CBOR unwrapping. Encountering these
features returns unsupported even inside an unevaluated body. Supported syntax
requires exact final `0*1` filler at the next byte boundary, complete input
consumption, valid UTF-8, complete primitive payloads, and profile-compatible
versions. Nonminimal natural encodings and noncanonical byte chunk lengths are
valid. Unknown UPLC versions are decode failures for this profile.

For unsupported syntax, the decoder checks builtin tags and complete constant
type headers, then stops. It does not claim to validate unimplemented payloads,
subsequent subterms, or final filler. Constr/case version gates are checked;
constr's tag and first field-list bit are read. Unsupported therefore means the
request cannot be assessed within this implementation, not that its remaining
encoding is known valid.

## Exact costs and failures

The checked-in profile is the explicit **350-parameter development model**, not
a mainnet snapshot. Every request supplies the entire hashed vector. Profile
IDs are labels and cannot select coefficients. Custom vectors with a correct
hash use their actual machine coefficients. Wrong vector length is an
infrastructure error. Negative machine coefficients, including reserved
builtin/constr/case events, are unsupported; negative builtin polynomial
coefficients elsewhere in the model are allowed.

Startup uses ledger positions 29/30. CPU/memory pairs for application, constant,
delay, force, lambda, and variable begin at 17, 21, 23, 25, 27, and 31. With the
supplied development model, startup costs 100 CPU/100 memory and each supported
compute event costs 16000 CPU/100 memory. A constant therefore costs exactly
16100 CPU/200 memory. Return transitions, explicit error, closure application,
and result discharge have no separate execution-unit cost.

The machine follows the pinned official CEK's default slippage of 200: startup
is immediate, compute events accumulate, the 200th event flushes before its
action, and successful halt flushes the remainder. Each flush charges accumulated
constants, variables, lambdas, applications, delays, then forces. Semantic errors
do not flush pending events. For example, `force(error)` with startup-only budget
is evaluation failure after consuming 100/100, not budget exhaustion from a
pending force charge. Charge order matters when a batch exhausts the limit.

Equality with the restricting limit succeeds. An exhausting charge includes
both full attempted dimensions, even beyond the supplied limit. Arithmetic uses
checked i128 multiplication/addition; an attempted total beyond the nonnegative
i64 wire range becomes budget exhaustion with a null budget. No clamping,
wrapping, default model, or large-budget imitation of counting is used. Malformed
input fails before startup with a null budget. Implementation limits return
unsupported and never create semantic failures or conformance passes.

## Portable implementation bounds

These are implementation limits, not ledger validity rules. Traversal, runtime
ownership, and result discharge use flat arenas and iterative stacks.

| Resource | Limit |
| --- | --- |
| Raw Flat input | 4 MiB (the JSON request also has an 8 MiB transport limit) |
| AST nodes / root-to-leaf depth | 100000 / 512, with a leaf at depth zero |
| Bytestring or UTF-8 string payload | 1 MiB per constant |
| Integer unsigned magnitude | 64 KiB |
| Runtime compute/return/discharge work and environment traversal | 10000000 work units |
| Aggregate runtime values, environments, and pending frames | 1000000 entries |
| Expanded normalized result nodes | 100000 |
| Normalized structural JSON depth | 128, including constant nesting |
| Normalized term serialized size | 8 MiB |

The work/allocation limits also apply under custom models with zero costs.
Node and Chromium/Firefox tests exercise successful evaluated AST depth 512,
successful result depth 128, and one beyond each limit as unsupported. They also
test exact consumed CPU/memory above JavaScript's integer precision and overflow.

## Corpus and provenance

`fixtures/milestone.jsonl` contains 68 strict cases: 34 selected official Plutus
fixtures, 18 manually derived CEK probes, and 16 exact/one-short/zero budget
boundaries. Its 48 successful cases include independent structural terms and
exact budgets. The 20 failures retain independent kinds; official failures do
not invent missing budget goldens. Both references and the candidate compare
failure costs when `--failure-costs` is requested.

Official raw inputs, expected results, and budget files are preserved unchanged
under `fixtures/milestone/plutus`, including hashes and raw texts in provenance.
Both pinned reference encoders must produce identical Flat bytes. Both reference
parsers must agree on structural normalization of official expected results.
Manual probes record independent expected syntax, explicitly counted CEK events,
and pinned specification/source hashes in `fixtures/milestone/manifest.json`.
The builder only invokes encoders and normalizers, never evaluator output to
derive goldens. It refuses overwrites. To reconstruct and verify every committed
byte after building the references:

```sh
python3 tools/build_milestone_corpus.py --check
make milestone-check
make milestone-reference-check
make test-wasm test-browser
BROWSER=firefox npm run test:browser
```

`fixtures/milestone-decoder.jsonl` supplies 18 independent candidate expectations
for malformed input, strict filler/consumption, version gates, zero indices, and
valid-but-open positive variables. They intentionally do not rely on reference
agreement: both raw reference APIs accept trailing bytes and unknown versions,
both decode zero variables, and pinned Aiken panics on a top-level variable one.
`fixtures/milestone-failure-policy-audit.jsonl` retains those variable-policy
probes and the startup-only `force(error)` charging probe. Run it with all engines
to expose reference limitations; it is not the strict acceptance corpus.

`fixtures/milestone-unsupported.jsonl` keeps seven deferred features visible.
Its Node/browser assertions require unsupported and are scope tests, not
conformance passes. `make report` reports these and broader smoke coverage with
`complete: false`; `make conformance` remains strict for the broader smoke corpus.
The full official textual corpus and its array/string/version disagreements,
normalization gaps, and reference parser/cost-arithmetic panics remain unchanged.
Do not rewrite goldens, weaken comparisons, or promote those cases to passing
coverage. Candidate report identity remains the package version; release
provenance still needs the built commit recorded separately.

The next evaluator milestone should add a separately specified builtin slice,
including builtin force/application rules, exact costs from the supplied vector,
and independent semantic/cost fixtures. Full text parsing and other deferred
features remain separate work.
