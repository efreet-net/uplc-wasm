# UPLC evaluator for Rust and WebAssembly

An independent Cardano UPLC evaluator with pinned Aiken and
Amaru reference adapters, official Plutus fixtures, and native/Wasm parity tests.

**The evaluator implements a small raw Flat subset:** arbitrary
precision integers, bytestrings, strings, booleans, unit, variables, lambdas and
application, delay and force, and explicit errors. Native and Wasm call the same
independent CEK machine, with exact costs and restricting budgets. Its builtin
slice includes `addInteger`, `subtractInteger`, `multiplyInteger`,
`divideInteger`, `quotientInteger`, `remainderInteger`, `modInteger`,
`equalsInteger`, `lessThanInteger`, `lessThanEqualsInteger`, and `ifThenElse`,
including forcing, partial application, and polymorphic returned values.
Other builtins, constr/case, composite constants, text parsing, counting mode, and
historical profiles remain explicitly unsupported. This is scoped conformance,
not a complete UPLC evaluator; see [milestone coverage and limits](docs/milestone.md).

## Quick start

Requirements: Rust 1.96.0, Python 3.12+, Node.js 24, and native build tools
(C/C++, CMake, pkg-config) for the references. The Amaru adapter uses its
upstream's exact `nightly-2026-09-04` toolchain; its declared stable MSRV alone
does not compile the pinned kernel code.

```sh
make check test milestone-check builtin-check division-check
make report

# Build the actual release Wasm artifact and exercise its JavaScript API.
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.129 --locked
make test-wasm

# Browser API parity, using the same Wasm artifact and fixture requests.
npm ci
npx playwright install --with-deps chromium firefox
make test-browser
BROWSER=firefox npm run test:browser
```

`make milestone-check` retains the original 68 semantic/cost cases and 18
independent decoder expectations. `make builtin-check` adds 162 builtin cases,
24 official/wire-policy cases, and 8 builtin decoder expectations.
`make builtin-all-check` compares the 68 + 162 cases strictly across native,
release Wasm, Aiken, Amaru, and independent goldens, including failure costs;
the separate decoder/policy checks remain strict native/Wasm gates.
`make division-all-check` retains those gates and adds 349 division-family
cases across all four engines plus 60 native/Wasm policy and decoder cases.
`make report` keeps broader smoke and deferred
feature coverage visible, reporting unsupported cases separately from passes.
`make conformance` still checks the broad smoke corpus strictly and remains
incomplete. Node and browser tests check committed independent goldens as well as
native/Wasm parity, including exact integers and consumed costs beyond JavaScript
number precision. Unsupported scope assertions are separate from conformance passes.

## Reference comparisons

```sh
rustup toolchain install nightly-2026-09-04 --profile minimal
make reference-check
make milestone-reference-check
make division-all-check generated-flat-check generated-division-flat-check

# Intentionally nonzero: preserves 23 mismatches and one reference error.
make builtin-reference-audit
# Intentionally nonzero: 30 division mismatches and 14 reference errors.
make division-reference-audit

python3 tools/conformance.py \
  --engine native=target/debug/uplc-native \
  --engine aiken=tools/oracle-aiken/target/debug/oracle-aiken \
  --engine amaru=tools/oracle-amaru/target/debug/oracle-amaru \
  --allow-unsupported

# Include the packaged Wasm module as another engine.
python3 tools/conformance.py \
  --engine native=target/debug/uplc-native \
  --engine 'wasm=node tools/wasm-oracle.mjs' \
  --allow-unsupported
```

`tools/upstreams.py` downloads immutable source revisions and verifies their
archive SHA-256 hashes before extracting into `.cache/upstreams`. Reuse also
checks the extracted source files against the archive and rejects modifications
or additions outside build `target` directories. It does not edit those sources.
Both adapter workspaces have independent Cargo lockfiles;
their dependencies never enter the Wasm build. Source pins are recorded in
`upstreams.lock.json`, not floating branches or crate names that could refer to
older Amaru distributions.

The initial compatibility profile is **PlutusV3, protocol 11, with the exact
350-parameter development model used by the pinned Plutus fixtures**. This is
not a mainnet protocol-parameter snapshot. Its provenance and parameter hash
are in `profiles/plutus-v3-pv11.json`. Other ledger/protocol combinations are
explicitly unsupported by these starter adapters.

## Layout

| Path | Purpose |
| --- | --- |
| `crates/uplc-core` | Independent evaluator implementation boundary; no oracle dependencies |
| `crates/uplc-wasm` | Thin `wasm-bindgen` export calling the same core |
| `crates/conformance` | Versioned JSONL protocol, validation, and native adapter server |
| `tools/conformance.py` | Persistent subprocess runner, deadlines, comparisons, failure artifacts |
| `tools/oracle-*` | Separately pinned native reference evaluators and AST normalizers |
| `fixtures/smoke.jsonl` | Nine small semantic, cost, trace, decoding, and budget cases |
| `fixtures/milestone*.jsonl` | Strict Flat milestone, independent decoder goldens, and visible deferred/audit cases |
| `fixtures/milestone/plutus` | Unmodified official subset inputs, expected results, and budgets |
| `fixtures/builtins*.jsonl`, `fixtures/builtins` | Builtin strict/policy/decoder/audit scopes, 39 unmodified official triples, and reconstruction manifest |
| `fixtures/division*.jsonl`, `fixtures/division` | Division strict/policy/decoder/audit scopes, 24 unmodified official triples, and reconstruction manifest |
| `fixtures/plutus` | Unmodified upstream seed inputs/goldens, with LICENSE and NOTICE |
| `profiles` | Explicit language/protocol/cost-model combinations |
| `fuzz` | Separate cargo-fuzz workspace with transport and raw Flat entry points |
| `tests` | Node and Chromium/Firefox tests of the packaged release Wasm |

The process runner is Python standard library only. It compares every engine
pair, so an unsupported candidate cannot hide a disagreement between references.
It validates complete response envelopes and nested normalized terms, rejecting
JSON numbers in integer positions, malformed constructors, and duplicate fields.
It writes `artifacts/conformance/report.json` and individual reproduction
artifacts for mismatches and infrastructure failures. Command arguments are
tokenized without invoking a shell and recorded with the comparison settings.
On POSIX, timeout cleanup kills the evaluator's process group so descendants
holding its pipes cannot defeat the deadline.

## Growing the corpus

```sh
make generated
python3 tools/conformance.py --corpus .cache/generated.jsonl \
  --engine aiken=tools/oracle-aiken/target/debug/oracle-aiken \
  --engine amaru=tools/oracle-amaru/target/debug/oracle-amaru

make import-plutus
python3 tools/conformance.py --corpus .cache/plutus.jsonl \
  --engine aiken=tools/oracle-aiken/target/debug/oracle-aiken \
  --engine amaru=tools/oracle-amaru/target/debug/oracle-amaru \
  --allow-unsupported

# Optional, requires cargo-fuzz and a nightly Rust toolchain.
cargo install cargo-fuzz --locked
cargo +nightly fuzz run wire -- -max_total_time=60
cargo +nightly fuzz run flat -- -max_total_time=60
```

Generated cases are closed arithmetic programs, with seeded choices of integer
boundaries and lambda/delay/force wrappers. Seeds and case indices are recorded.
`make generated-flat-check` checks the same 1,000 seeded cases across native,
release Wasm, and both references. `--flat` requires both reference encoders to
agree and records source bytes, hashes, encoder identities, and an independently
derived CEK/builtin charge ledger. It preserves the generator's mathematical
expectations and adds exact costs without asking an evaluator to produce a
golden. Generation refuses overwrites; `--check` reconstructs existing output.
`make generated-division-flat-check` adds another 1,000 seeded Flat cases using
`--division`, with exact integer division, sign rules, and zero-divisor charging
derived independently. The original generator output remains unchanged.
They currently have no automatic shrinker; minimize disagreements and save them
under `fixtures/regressions` using the same JSONL format. Store real scripts
with already-applied arguments and their historical profiles under
`fixtures/replay`. A future ledger wrapper should separately test construction
of datum/redeemer/script-context arguments and ledger return-value rules.

The importer preserves official output and cost expectations, including raw
golden text and hashes in each case's provenance. Both reference adapters'
`--normalize` modes parse the expected term into a name-free AST without
evaluating it. A normalized golden is used only when both parsers agree;
unsupported normalization or parser disagreement remains incomplete coverage.
This matters for escaped strings that the pinned Aiken parser misreads. Custom
normalizers require two distinct implementations via repeated `--normalizer`.
Imports publish atomically, refuse overwrites, and require the fixture cost model.
`make provenance` verifies the profile coefficients and vendored seed files
against their pinned upstream sources; reference CI runs this check.
The pinned fixture revision primarily supplies textual UPLC; the seed corpus also includes an
explicit raw Flat case. Import newer Flat fixture sets under a new pinned profile.
The full corpus is an audit and may report upstream disagreements: for example,
the pinned Aiken parser rejects the valid array-constant fixtures. These are
reported as mismatches, not silently skipped or rewritten into passing goldens.

## Implementation boundaries

- Core/Wasm have no oracle dependencies. The implemented CEK/builtin slice
  uses arbitrary-precision integers and checked budget arithmetic independent
  of host pointer size. Costs come from the complete validated supplied vector;
  changing its hash and coefficients changes execution costs, regardless of ID.
- Division rounds down; quotient truncates toward zero. Nonzero modulo follows
  the divisor's sign and remainder the numerator's sign. Correctly typed,
  in-range zero-divisor calls incur their builtin charge before evaluation fails.
  Exact polynomial costs preserve signed cancellation, branch conditions, and
  official minima; portable work limits also apply under zero/custom models.
- Both starter references expose restricting evaluation. Counting requests
  return `unsupported`; a large budget is never labeled counting mode. The
  full importer uses a stated large restricting budget for counting-mode
  goldens, so budget exhaustion can legitimately produce a reported mismatch.
- Reference normalization covers all UPLC term constructors and primitive/list/pair
  constants. Data, BLS element results, native Value, and Amaru arrays need
  additional structural normalizers. Evaluations returning these values are
  marked unsupported instead of comparing debug strings. Crypto builtins that
  return primitive results can already be compared.
- Successes compare structural results, exact costs, and ordered traces.
  Failure diagnostics are retained but not compared verbatim. Partial failure
  costs are compared only with `--failure-costs`, because charging boundaries
  can differ between machines. Raw VM evaluation is distinct from ledger
  phase-two validation; the adapters currently use each library's raw APIs.
  These raw APIs accept trailing Flat bytes and unknown program versions in
  simple probes. Reference agreement therefore does not establish ledger decoder
  compliance. The candidate enforces full consumption, exact filler and the
  initial profile's version gates against independent decoder goldens. CBOR
  wrapping is outside this raw Flat API. See the milestone document for retained
  raw-reference De Bruijn disagreements and failure-charging policy.
- Haskell live evaluation, historical profiles, full Flat/CBOR decoder
  coverage, automatic shrinking, and performance benchmarks are follow-up
  work. The official fixture expectations are the current independent anchor.

See [the wire contract](docs/protocol.md). Primary upstream references:
[Plutus conformance](https://github.com/IntersectMBO/plutus/tree/5f785edeac0d1d89622d44344fdda07ef48e8c73/plutus-conformance),
[Aiken](https://github.com/aiken-lang/aiken/tree/b5c34839ee33a2d608a071f3679410f63ad68bae/crates/uplc),
[Amaru](https://github.com/pragma-org/amaru/tree/34a453005bcaaf837ee73bd996b99eab8ef92961/crates/amaru-uplc),
and the [UPLC specification](https://plutus.cardano.intersectmbo.org/resources/plutus-core-spec.pdf).
