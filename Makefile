PYTHON ?= python3
WASM_BINDGEN ?= wasm-bindgen

.PHONY: check test native upstreams references conformance report reference-check provenance wasm test-wasm test-browser generated generated-flat generated-flat-check generated-division-flat generated-division-flat-check import-plutus milestone-check milestone-reference-check builtin-check builtin-reference-check builtin-all-check builtin-reference-audit division-check division-reference-check division-all-check division-reference-audit

check:
	cargo fmt --all --check
	rustfmt --edition 2024 --check tools/oracle-aiken/src/main.rs tools/oracle-amaru/src/main.rs
	cargo clippy --workspace --all-targets --locked -- -D warnings

test:
	cargo test --workspace --locked
	$(PYTHON) -m unittest discover -s tools -p 'test_*.py' -v

native:
	cargo build --locked -p uplc-core --bin uplc-native

upstreams:
	$(PYTHON) tools/upstreams.py

references: upstreams
	cargo +1.96.0 build --locked --manifest-path tools/oracle-aiken/Cargo.toml
	cargo +nightly-2026-09-04 build --locked --manifest-path tools/oracle-amaru/Cargo.toml

# Broad smoke coverage remains strict and incomplete until later milestones.
conformance: native
	$(PYTHON) tools/conformance.py --engine native=target/debug/uplc-native

# Strict independently expected coverage of the implemented evaluator slice.
milestone-check: native
	$(PYTHON) tools/conformance.py --corpus fixtures/milestone.jsonl --engine native=target/debug/uplc-native --artifacts artifacts/milestone/native
	$(PYTHON) tools/conformance.py --corpus fixtures/milestone-decoder.jsonl --engine native=target/debug/uplc-native --artifacts artifacts/milestone/decoder

milestone-reference-check: native references provenance
	$(PYTHON) tools/build_milestone_corpus.py --check
	$(PYTHON) tools/conformance.py --corpus fixtures/milestone.jsonl --engine native=target/debug/uplc-native --engine aiken=tools/oracle-aiken/target/debug/oracle-aiken --engine amaru=tools/oracle-amaru/target/debug/oracle-amaru --failure-costs --artifacts artifacts/milestone/references

builtin-check: native
	$(PYTHON) tools/conformance.py --corpus fixtures/builtins.jsonl fixtures/builtins-candidate.jsonl fixtures/builtins-decoder.jsonl --engine native=target/debug/uplc-native --artifacts artifacts/builtins/native

builtin-reference-check: native references provenance
	$(PYTHON) tools/build_builtin_corpus.py --check
	$(PYTHON) tools/conformance.py --corpus fixtures/builtins.jsonl --engine native=target/debug/uplc-native --engine aiken=tools/oracle-aiken/target/debug/oracle-aiken --engine amaru=tools/oracle-amaru/target/debug/oracle-amaru --failure-costs --artifacts artifacts/builtins/references

# All four engines must match the unchanged first milestone and builtin slice.
# Independent decoder and official/wire-policy goldens remain strict candidate
# checks where the raw reference APIs demonstrably disagree with those rules.
builtin-all-check: native wasm references provenance
	$(PYTHON) tools/build_milestone_corpus.py --check
	$(PYTHON) tools/build_builtin_corpus.py --check
	$(PYTHON) tools/conformance.py --corpus fixtures/milestone.jsonl fixtures/builtins.jsonl --engine native=target/debug/uplc-native --engine 'wasm=node tools/wasm-oracle.mjs' --engine aiken=tools/oracle-aiken/target/debug/oracle-aiken --engine amaru=tools/oracle-amaru/target/debug/oracle-amaru --failure-costs --artifacts artifacts/builtins/all-engines
	$(PYTHON) tools/conformance.py --corpus fixtures/milestone-decoder.jsonl fixtures/builtins-candidate.jsonl fixtures/builtins-decoder.jsonl --engine native=target/debug/uplc-native --engine 'wasm=node tools/wasm-oracle.mjs' --failure-costs --artifacts artifacts/builtins/independent-policies

# Deliberately failing audit: 23 semantic/cost mismatches and one reference
# infrastructure error. Never reinterpret these records as conformance passes.
builtin-reference-audit: native wasm references
	$(PYTHON) tools/conformance.py --corpus fixtures/builtins-reference-audit.jsonl --engine native=target/debug/uplc-native --engine 'wasm=node tools/wasm-oracle.mjs' --engine aiken=tools/oracle-aiken/target/debug/oracle-aiken --engine amaru=tools/oracle-amaru/target/debug/oracle-amaru --failure-costs --artifacts artifacts/builtins/reference-audit

division-check: native
	$(PYTHON) tools/conformance.py --corpus fixtures/division.jsonl fixtures/division-candidate.jsonl fixtures/division-decoder.jsonl --engine native=target/debug/uplc-native --failure-costs --artifacts artifacts/division/native

division-reference-check: native references provenance
	$(PYTHON) tools/build_division_corpus.py --check
	$(PYTHON) tools/conformance.py --corpus fixtures/division.jsonl --engine native=target/debug/uplc-native --engine aiken=tools/oracle-aiken/target/debug/oracle-aiken --engine amaru=tools/oracle-amaru/target/debug/oracle-amaru --failure-costs --artifacts artifacts/division/references

# Retain all 230 strict and 50 candidate regression cases, then add 349 strict
# division cases and 60 independently justified candidate/decoder expectations.
division-all-check: builtin-all-check
	$(PYTHON) tools/build_division_corpus.py --check
	$(PYTHON) tools/conformance.py --corpus fixtures/division.jsonl --engine native=target/debug/uplc-native --engine 'wasm=node tools/wasm-oracle.mjs' --engine aiken=tools/oracle-aiken/target/debug/oracle-aiken --engine amaru=tools/oracle-amaru/target/debug/oracle-amaru --failure-costs --artifacts artifacts/division/all-engines
	$(PYTHON) tools/conformance.py --corpus fixtures/division-candidate.jsonl fixtures/division-decoder.jsonl --engine native=target/debug/uplc-native --engine 'wasm=node tools/wasm-oracle.mjs' --failure-costs --artifacts artifacts/division/independent-policies

# This remains a failing audit: 30 mismatches and 14 reference errors. Neither
# unsupported policies nor reference failures can be counted as strict passes.
division-reference-audit: native wasm references
	$(PYTHON) tools/conformance.py --corpus fixtures/division-reference-audit.jsonl --engine native=target/debug/uplc-native --engine 'wasm=node tools/wasm-oracle.mjs' --engine aiken=tools/oracle-aiken/target/debug/oracle-aiken --engine amaru=tools/oracle-amaru/target/debug/oracle-amaru --failure-costs --artifacts artifacts/division/reference-audit

# Broader coverage reporting permits unsupported cases, never mismatches.
report: native
	$(PYTHON) tools/conformance.py --engine native=target/debug/uplc-native --allow-unsupported
	$(PYTHON) tools/conformance.py --corpus fixtures/division-unsupported.jsonl --engine native=target/debug/uplc-native --allow-unsupported --artifacts artifacts/division/unsupported

provenance: upstreams
	$(PYTHON) tools/verify_provenance.py

reference-check: references provenance
	$(PYTHON) tools/conformance.py --engine aiken=tools/oracle-aiken/target/debug/oracle-aiken --engine amaru=tools/oracle-amaru/target/debug/oracle-amaru --artifacts artifacts/references

wasm:
	cargo build --locked --release -p uplc-wasm --target wasm32-unknown-unknown
	$(WASM_BINDGEN) --target nodejs --out-dir pkg/node target/wasm32-unknown-unknown/release/uplc_wasm.wasm
	$(WASM_BINDGEN) --target web --out-dir pkg/web target/wasm32-unknown-unknown/release/uplc_wasm.wasm

test-wasm: native wasm
	node --test tests/wasm.test.mjs

test-browser: native wasm
	npm run test:browser

generated:
	$(PYTHON) tools/generate_cases.py --seed 42 --count 1000 $(if $(wildcard .cache/generated.jsonl),--check,)

generated-flat: references provenance
	$(PYTHON) tools/generate_cases.py --flat --seed 42 --count 1000 --output .cache/builtin-generated.jsonl $(if $(wildcard .cache/builtin-generated.jsonl),--check,)

generated-flat-check: native wasm generated-flat
	$(PYTHON) tools/conformance.py --corpus .cache/builtin-generated.jsonl --engine native=target/debug/uplc-native --engine 'wasm=node tools/wasm-oracle.mjs' --engine aiken=tools/oracle-aiken/target/debug/oracle-aiken --engine amaru=tools/oracle-amaru/target/debug/oracle-amaru --failure-costs --artifacts artifacts/builtins/generated

generated-division-flat: references provenance
	$(PYTHON) tools/generate_cases.py --flat --division --seed 42 --count 1000 --output .cache/division-generated.jsonl $(if $(wildcard .cache/division-generated.jsonl),--check,)

generated-division-flat-check: native wasm generated-division-flat
	$(PYTHON) tools/conformance.py --corpus .cache/division-generated.jsonl --engine native=target/debug/uplc-native --engine 'wasm=node tools/wasm-oracle.mjs' --engine aiken=tools/oracle-aiken/target/debug/oracle-aiken --engine amaru=tools/oracle-amaru/target/debug/oracle-amaru --failure-costs --artifacts artifacts/division/generated

import-plutus: references provenance
	$(PYTHON) tools/import_plutus.py
