# Integer division evaluator milestone

The core independently decodes and evaluates raw Flat UPLC 1.0.0 and 1.1.0 under
PlutusV3/protocol 11. It supports one-based De Bruijn variables,
lambda/application, delay/force, explicit error, primitive integer,
bytestring, UTF-8 string, boolean, and unit constants, and the eleven builtins
below. Integers use arbitrary precision; normalized integers and budgets cross
JSON and JavaScript as decimal strings. Native and release Wasm call the same
independent core and report its build identity in the existing `revision` field.

| Builtin | Flat tag | Required forces | Term arguments |
| --- | --- | --- | --- |
| addInteger | 0 | 0 | integer, integer |
| subtractInteger | 1 | 0 | integer, integer |
| multiplyInteger | 2 | 0 | integer, integer |
| divideInteger | 3 | 0 | integer, integer |
| quotientInteger | 4 | 0 | integer, integer |
| remainderInteger | 5 | 0 | integer, integer |
| modInteger | 6 | 0 | integer, integer |
| equalsInteger | 7 | 0 | integer, integer |
| lessThanInteger | 8 | 0 | integer, integer |
| lessThanEqualsInteger | 9 | 0 | integer, integer |
| ifThenElse | 26 | 1 | boolean, any value, any value |

These are original-batch builtins available in the supported profile. Metadata,
denotations, force/application rules, memory usage, and cost expressions are
derived from the pinned official Plutus sources recorded in
`fixtures/builtins/manifest.json` and `fixtures/division/manifest.json`, and
cross-checked against both independent reference adapters. Core and Wasm have
no oracle dependencies.

## Values, forcing, and integer semantics

The CEK machine evaluates the function before the argument and evaluates all
arguments strictly. Builtins collect forces and arguments as immutable values.
Partial applications remain values, including wrong-typed or out-of-range
arguments until saturation. Application before a required force, a force after
all required forces have been consumed, and applying a non-function are
evaluation failures. At full saturation, argument validation precedes costing.

`ifThenElse` selects an already-evaluated branch. Both branches are evaluated
before selection; selecting a delayed branch becomes lazy only when the caller
subsequently forces that delay. The selected value can be a primitive, a captured
closure, a delay, or a partial builtin. Overapplication follows that returned
value: a returned function accepts a further argument, while a returned integer
does not. Captured environments survive selection and partial application.

Discharge reconstructs builtin tags, consumed forces, and collected applications
as structural syntax. It substitutes captured values with correct De Bruijn
indices and never evaluates or optimizes underneath returned lambdas or delays.
Open positive variables and explicit errors remain evaluation failures. Index
zero violates Flat's positive variable-index rule and fails decoding.

Protocol 11 selects official builtin semantics variant E. The inputs to
addition, subtraction, multiplication, all four division operations, and the
two ordering comparisons must be in **[-2^262143, 2^262143 - 1]**.
Violations are semantic evaluation failures
at saturation. `equalsInteger`, primitive constants, and arithmetic results do
not have this semantic bound; they remain subject to the separate implementation
integer limit below. Thus a valid arithmetic result can lie outside the input
range and a later arithmetic call can reject it. This distinction is covered by
independent cases, including the negative endpoint and positive one-past-end.

For nonzero `b`, `divideInteger a b` is the floor of the exact rational `a/b`;
`quotientInteger a b` truncates toward zero. Remainder is `a - b*quotient(a,b)`
and modulo is `a - b*divide(a,b)`. A nonzero remainder has the numerator's sign;
a nonzero modulo has the divisor's sign. For example, `-7 / 3` produces divide
`-3`, quotient `-2`, remainder `-1`, and modulo `2`. No floating-point division
is used. All four independently enforce both argument bounds. Dividing the
minimum permitted input by `-1` produces the permitted result `2^262143` for
divide/quotient and zero for remainder/modulo. Zero divisors fail during the
denotation, after argument validation, charging, and the work debit.

Integer memory is `max(1, ceil(bit_length(abs(n)) / 64))`: zero consumes one
unit, signs do not add a word, and 2^64 consumes two units. This follows pinned
`ExMemoryUsage.hs`, independently of host pointer width or BigInt's internal limb
representation. The same measurements are used on native and wasm32.

Other builtins, constr/case evaluation, complex constant payloads, textual UPLC,
genuine counting, historical profiles, ledger argument construction, CBOR
wrapping, tracing, and performance optimization remain outside this milestone.
Unimplemented features return unsupported even in unevaluated bodies. Supported
syntax requires complete payloads, exact final `0*1` filler, full consumption,
valid UTF-8, positive variable indices, and profile-compatible versions.
Nonminimal natural encodings and noncanonical byte chunk lengths remain valid.
For unsupported syntax the decoder checks its implemented headers/version gates,
then stops; it does not claim to validate unimplemented payloads or trailing
syntax. Unsupported outcomes never count as conformance passes.

## Exact costs and failure ordering

The checked-in profile is the explicit **350-parameter development model**, not
a mainnet snapshot. Every request supplies the complete hashed vector; profile
IDs are descriptive labels. Wrong vector length is an infrastructure error.
Negative machine coefficients, including reserved constr/case events, are
unsupported. Negative builtin polynomial coefficients are valid and are used
as supplied. Only a negative *computed charge* is unsupported; it is never
clamped, wrapped, or used to credit the budget.

Startup uses ledger positions 29/30. CPU/memory pairs for application, builtin,
constant, delay, force, lambda, and variable begin at 17, 19, 21, 23, 25, 27, and
31. Startup is 100 CPU/100 memory and each supported compute event is
16000 CPU/100 memory in the supplied model. A bare builtin costs 16100/200.
Return transitions, closure application, explicit error, and discharge have no
separate execution-unit cost.

Let `x` and `y` be the argument memory sizes, `M=max(x,y)`, and `m=min(x,y)`.
The following application charges are additional to machine events. The listed
positions are zero-based; custom models use the same formulas with their actual
coefficients.

| Builtin | CPU positions and supplied expression | Memory positions and supplied expression |
| --- | --- | --- |
| addInteger | 0,1: `100788 + 420*M` | 2,3: `1 + M` |
| subtractInteger | 167,168: `100788 + 420*M` | 169,170: `1 + M` |
| multiplyInteger | 124,125: `90434 + 519*x*y` | 126,127: `x+y` |
| equalsInteger | 71,72: `51775 + 558*m` | 73: `1` |
| lessThanInteger | 99,100: `44749 + 541*m` | 101: `1` |
| lessThanEqualsInteger | 96,97: `43285 + 552*m` | 98: `1` |
| ifThenElse | 84: `76049` | 85: `1` |

For division, define
`P(u,v)=max(85848, 123203 + 7305*v - 900*v*v + 1716*u + 960*u*v + 57*u*u)`.
Each family block stores the constant branch, `c00`, `c01`, `c02`, `c10`,
`c11`, `c20`, and polynomial minimum, in that order. The next positions hold
the memory parameters. All coefficients come from the supplied vector.

| Builtin | CPU positions and supplied expression | Memory positions and supplied expression |
| --- | --- | --- |
| divideInteger | 49–56: `P(M,m)`; position 49 is ignored | 57–59: `max(1,x-y)` |
| quotientInteger | 130–137: `85848` if `x<y`, otherwise `P(x,y)` | 138–140: `max(1,x-y)` |
| remainderInteger | 141–148: `85848` if `x<y`, otherwise `P(x,y)` | 149–150: `y` |
| modInteger | 114–121: `P(M,m)`; position 114 is ignored | 122–123: `y` |

The CPU branch boundary compares memory sizes, not integer magnitudes or signs.
Equal sizes use the polynomial. Divide/modulo sort CPU sizes even above the
diagonal and never use the stored constant. Memory always uses the original
ordered sizes. For custom models, divide/quotient memory is
`intercept + slope*max(minimum,x-y)`; remainder/modulo memory is
`intercept + slope*y`. The signed difference and minimum are applied before
multiplication. Polynomial minima are applied only to the polynomial branch;
no extra zero clamp is introduced. Exact intermediates preserve cancellation
before the final charge is checked, including opposite quadratic terms that
individually exceed the budget representation.

Startup charges immediately. Compute events follow the pinned official CEK's
200-event slippage policy: the 200th event flushes before its action; successful
halt flushes the remainder. A flush charges constants, variables, lambdas,
applications, delays, forces, then builtin machine events. Semantic errors do
not flush an unfinished batch.

Saturation first validates argument types and the profile's integer input
bounds. If valid, it charges the entire builtin CPU/memory pair immediately,
without flushing pending CEK events. It then debits independent implementation
work and executes the denotation. Wrong types, input-range errors, incorrect
forcing, and partial applications have no builtin application charge. Charges
already submitted remain consumed. For example, overapplying an integer result
retains its builtin charge but leaves an unfinished machine batch unflushed.
The corpus independently tests builtin exhaustion before a pending batch,
failure at a 200-event crossing, successful flushes, and semantic-error order.
For one-word integers, a zero-divisor call consumes startup plus the application
charge, `132441` CPU and `101` memory under the supplied model; its five pending
CEK events are not flushed. Startup, a completed CEK batch, and the builtin
charge can each exhaust first and prevent the zero-divisor failure. The corpus
tests each precedence, including exactly 200 compute events before execution.

Equality with a restricting limit succeeds. An exhausting charge includes both
full attempted dimensions. Costs use checked wide arithmetic; if exact attempted
consumption cannot fit the wire's nonnegative i64 budget, the outcome is
`budget_exhausted` with a null budget. It never wraps or saturates. Signed
coefficients can cancel to a valid nonnegative charge before range checking.
Malformed Flat fails before startup with null budget. Implementation limits
return unsupported. The 8 MiB UTF-8 JSON body limit still excludes the framing
LF; native EOF-terminated final records work and overlong records terminate
without unbounded draining. See [the protocol](protocol.md) for framing and
build-identity details.

## Portable implementation bounds

These are implementation limits, separate from ledger validity and execution
units. Traversal, runtime ownership, and discharge use arenas and iterative
stacks. Input constants are borrowed; computed constants are owned.

| Resource | Limit |
| --- | --- |
| Raw Flat input | 4 MiB; JSON bodies separately limited to 8 MiB |
| AST nodes / root-to-leaf depth | 100000 / 512, leaf at depth zero |
| Bytestring or UTF-8 string payload | 1 MiB per constant |
| Integer unsigned magnitude, including computed results | 64 KiB |
| Compute/return/discharge work, environment traversal, builtin work | 10000000 units |
| Aggregate runtime values, environments, pending frames | 1000000 entries |
| Aggregate generated constant payload retained by the runtime | 8 MiB |
| Expanded normalized result nodes | 100000 |
| Normalized structural JSON depth | 128, including constant nesting |
| Normalized term serialized size | 8 MiB |

Portable builtin work uses `max(x,y)` for integer linear/comparison operations,
`x*y` for multiplication, `max(x,y)+x*y` for all four division operations, and
one unit for branch selection. These conservative division units use 64-bit
magnitude words even for zero, equal operands, or a numerator smaller than its
divisor; no host limb size or arithmetic fast path changes the bound. Work is
debited after the execution-unit charge and before arithmetic. Results are checked
against the integer and cumulative generated-payload limits; one rejected
transient integer is bounded by the 64 KiB individual cap. Current semantics-E
input bounds already keep a single legal arithmetic result within that cap.
Zero/custom cost models remain subject to every implementation bound.

Native, Node, Chromium, and Firefox checks include exact integer/cost strings
beyond JavaScript precision, signed coefficient cancellation, CPU and memory
overflow, a legal multiplication exceeding the work cap, and balanced addition
trees on either side of the actual generated-payload cap under a zero model.
The successful tree's result is derived independently with integer arithmetic.
Unsupported limit checks assert the intended resource reason.
Division probes additionally straddle the work cap with 3161-word and 3162-word
legal equal operands under zero/custom models, verify that charge exhaustion
precedes the work limit, and accumulate divide/quotient results on either side
of the generated-payload cap using a single captured input.

## Corpus, provenance, and acceptance

The original files remain unchanged: `fixtures/milestone.jsonl` has 68 cases
(34 official, 18 manual CEK probes, 16 budget boundaries), and
`fixtures/milestone-decoder.jsonl` has 18 independent decoder expectations.
Original official raw inputs, results, and budgets remain authoritative.

| Retained builtin corpus | Cases | Acceptance scope |
| --- | --- | --- |
| `builtins.jsonl` | 162 | Strict native, release Wasm, Aiken, Amaru, and independent goldens, including failure costs |
| `builtins-candidate.jsonl` | 24 | Strict native/Wasm official semantics and explicit wire policies where references disagree |
| `builtins-decoder.jsonl` | 8 | Strict independent supported-builtin decoding expectations |
| `builtins-reference-audit.jsonl` | 24 | Deliberately failing audit of the same policy cases: 23 mismatches, 1 reference infrastructure error |
| `builtins-unsupported.jsonl` | 14 | Historical deferred records, retained unchanged; five now explicitly graduated |

Current unsupported reporting uses the division scope below.

| Division corpus | Cases | Acceptance scope |
| --- | --- | --- |
| `division.jsonl` | 349 | Strict native, release Wasm, Aiken, Amaru, and independent goldens, including failure costs |
| `division-candidate.jsonl` | 44 | Strict native/Wasm official semantics and explicit arithmetic policies where references disagree |
| `division-decoder.jsonl` | 16 | Strict independent division Flat decoding expectations |
| `division-reference-audit.jsonl` | 44 | Deliberately failing audit: 30 mismatches and 14 reference infrastructure errors |
| `division-unsupported.jsonl` | 21 | Nine retained unsupported features, eight negative-charge policies, four work-limit probes; zero conformance passes |

There are 39 unchanged official input/result/budget triples under
`fixtures/builtins/plutus`, with source bytes, hashes, and raw golden texts
preserved, plus 24 division triples under `fixtures/division/plutus`.
Both pinned reference encoders must agree on derived Flat bytes;
both independent parsers must agree before official expected terms are
normalized. Manual cases retain source anchors, independent mathematical
results, and explicit charge-event ledgers. Builders invoke encoding and parsing
only, never candidate or reference evaluation to derive expectations. The
seeded arithmetic generator's `--flat` mode preserves its independent results
and adds independently computed exact costs, source hashes, and both encoders'
identities. Generation refuses overwrites; reconstruction uses `--check`.
The optional `--division` generator extends the original arithmetic choices
with all four operations, independent integer rounding and sign rules, and
zero-divisor failure ledgers. Its separate 1,000-case gate preserves the original
1,000 generated cases and their exact expectations.

The original seven deferred records remain unchanged. Bare `addInteger` is
deliberately graduated as `builtins/probe/bare-addInteger`, retaining the original
record hash, source bytes, and complete provenance. The six remaining records
were copied verbatim into the builtin unsupported corpus alongside eight
visibility probes. This milestone graduates the four bare division builtin
records and the divideInteger-under-lambda record into strict division goldens;
each preserves its original record and provenance. The nine still-deferred
records are copied verbatim into the division unsupported corpus. Eight
negative-charge policies and four legal-input work probes are additional
unsupported assertions. No broader disagreement or unsupported outcome is
promoted to a pass.

```sh
make check test milestone-check builtin-check division-check report
make division-all-check generated-flat-check generated-division-flat-check
make test-wasm test-browser
BROWSER=firefox npm run test:browser
python3 tools/build_milestone_corpus.py --check
python3 tools/build_builtin_corpus.py --check
python3 tools/build_division_corpus.py --check
make provenance

# Intentionally nonzero, with reproduction artifacts preserved:
make builtin-reference-audit
make division-reference-audit
```

`builtin-all-check` strictly compares 230 semantic/cost cases across all four
engines and 50 decoder/policy cases across native/Wasm. The separate generated
gate adds 1,000 seeded Flat cases across all four. `division-all-check` retains
these gates and adds 349 strict cases and 60 native/Wasm decoder/policy cases,
for totals of 579 and 110. The division generated gate adds another 1,000 Flat
cases. CI uploads both deliberately failing audits and requires their exact
case IDs, categories, and summary counts.

Aiken charges saturated wrong-type calls before rejecting them, contrary to
pinned official `Builtin/Meaning.hs`; Amaru agrees with the official no-charge
policy. Aiken also omits variant E's integer input range; Amaru enforces it.
These produce 16 type/failure-charge mismatches and seven input-range mismatches.
For the custom overflow policy, pinned Aiken panics in cost arithmetic and Amaru
saturates CPU consumption to i64::MAX; the candidate's explicit checked policy
returns budget exhaustion with null budget. The audit retains all 24 cases and
their independent expectations unchanged.

The division audit retains 44 independently justified cases: eight wrong-type
and twenty input-bound cases, four exact-zero and four positive cancellation
cases, and eight CPU/memory overflow cases. Aiken can panic on polynomial and
memory arithmetic; Amaru can saturate intermediate terms, losing a valid
positive cancellation result, or clamp unrepresentable charges. These yield
30 mismatches and 14 reference infrastructure errors, with zero passes. The
same official/policy expectations remain strict native/Wasm gates. Signed
coefficients and valid cancellation are not rejected to match either reference.

Broader coverage remains incomplete. Both raw references accept some trailing
Flat bytes, unknown versions, and zero variable indices; Aiken panics on a raw
top-level variable-one probe. The retained milestone failure-policy audit and
independent decoder goldens preserve those disagreements. The full official
text corpus still exposes array/string/version disagreements, parser and
cost-arithmetic panics, and unavailable normalizations. The builtin milestone
does not make the candidate permissive to match them.

The next recommended milestone is the basic bytestring family, with exact
size-dependent costs, index/slice boundaries, failure charging, and portable
allocation limits anchored by independent official fixtures. Other builtin
families and text parsing remain separate work.
