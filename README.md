# UPLC evaluator for Rust and WebAssembly

A scaffold for an independent Cardano UPLC evaluator with pinned Aiken and
Amaru reference adapters, official Plutus fixtures, and native/Wasm parity tests.

**The candidate evaluator is not implemented yet.** It returns `unsupported` for
valid evaluation requests. No semantic or costing compliance is claimed. The
reference adapters and harness work independently of the candidate, so they can
be used to guide its implementation.

## Quick start

Requirements: Rust 1.96.0, Python 3.12+, Node.js 24, and native build tools
(C/C++, CMake, pkg-config) for the references. The Amaru adapter uses its
upstream's exact `nightly-2026-09-04` toolchain; its declared stable MSRV alone
does not compile the pinned kernel code.

```sh
make check test
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

`make report` explicitly permits unsupported cases and reports them separately
from passes. `make conformance` is strict and exits unsuccessfully while any case
is unsupported, mismatched, or affected by an infrastructure error. CI currently
uses reporting mode for the candidate; remove `--allow-unsupported` when its
initial compatibility profile is implemented. Browser/Node API parity checks
are infrastructure checks, not a claim of UPLC conformance.

## Reference comparisons

```sh
rustup toolchain install nightly-2026-09-04 --profile minimal
make references
make reference-check

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
archive SHA-256 hashes before extracting into `.cache/upstreams`. It does not
edit those sources. Both adapter workspaces have independent Cargo lockfiles;
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
| `fixtures/plutus` | Unmodified upstream seed inputs/goldens, with LICENSE and NOTICE |
| `profiles` | Explicit language/protocol/cost-model combinations |
| `fuzz` | Separate cargo-fuzz workspace with transport and raw Flat entry points |
| `tests` | Node and Chromium/Firefox tests of the packaged release Wasm |

The process runner is Python standard library only. It compares every engine
pair, so an unsupported candidate cannot hide a disagreement between references.
It writes `artifacts/conformance/report.json` and individual reproduction
artifacts for mismatches and infrastructure failures. Command arguments are
tokenized without invoking a shell.

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
They currently have no automatic shrinker; minimize disagreements and save them
under `fixtures/regressions` using the same JSONL format. Store real scripts
with already-applied arguments and their historical profiles under
`fixtures/replay`. A future ledger wrapper should separately test construction
of datum/redeemer/script-context arguments and ledger return-value rules.

The importer preserves official output and cost expectations. A reference
adapter's `--normalize` mode only parses the expected term into a name-free AST;
it does not evaluate the program or regenerate its costs. Unsupported golden
normalization remains visible as incomplete coverage. The pinned fixture
revision primarily supplies textual UPLC; the seed corpus also includes an
explicit raw Flat case. Import newer Flat fixture sets under a new pinned profile.
The full corpus is an audit and may report upstream disagreements: for example,
the pinned Aiken parser rejects the valid array-constant fixtures. These are
reported as mismatches, not silently skipped or rewritten into passing goldens.

## Implementation boundaries

- Implement Flat decoding, version gates, the CEK machine, builtins, and exact
  costing in `uplc-core`. Use arbitrary-precision UPLC integers and explicit
  integer budget arithmetic independent of host pointer size.
- Both starter references expose restricting evaluation. Counting requests
  return `unsupported`; a large budget is never labeled counting mode. The
  full importer uses a stated large restricting budget for counting-mode
  goldens, so budget exhaustion can legitimately produce a reported mismatch.
- Normalization covers all UPLC term constructors and primitive/list/pair
  constants. Data, BLS element results, native Value, and Amaru arrays need
  additional structural normalizers. Evaluations returning these values are
  marked unsupported instead of comparing debug strings. Crypto builtins that
  return primitive results can already be compared.
- Successes compare structural results, exact costs, and ordered traces.
  Failure diagnostics are retained but not compared verbatim. Partial failure
  costs are compared only with `--failure-costs`, because charging boundaries
  can differ between machines. Raw VM evaluation is distinct from ledger
  phase-two validation; the adapters currently use each library's raw APIs.
- Haskell live evaluation, historical profiles, full Flat/CBOR decoder
  coverage, automatic shrinking, and performance benchmarks are follow-up
  work. The official fixture expectations are the current independent anchor.

See [the wire contract](docs/protocol.md). Primary upstream references:
[Plutus conformance](https://github.com/IntersectMBO/plutus/tree/5f785edeac0d1d89622d44344fdda07ef48e8c73/plutus-conformance),
[Aiken](https://github.com/aiken-lang/aiken/tree/b5c34839ee33a2d608a071f3679410f63ad68bae/crates/uplc),
[Amaru](https://github.com/pragma-org/amaru/tree/34a453005bcaaf837ee73bd996b99eab8ef92961/crates/amaru-uplc),
and the [UPLC specification](https://plutus.cardano.intersectmbo.org/resources/plutus-core-spec.pdf).
