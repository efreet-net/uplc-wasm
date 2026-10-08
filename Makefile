PYTHON ?= python3
WASM_BINDGEN ?= wasm-bindgen

.PHONY: check test native upstreams references conformance report reference-check provenance wasm test-wasm test-browser generated import-plutus

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

# Strict by default. This fails until the candidate implements the smoke corpus.
conformance: native
	$(PYTHON) tools/conformance.py --engine native=target/debug/uplc-native

# Scaffold coverage reporting explicitly permits unsupported cases, never mismatches.
report: native
	$(PYTHON) tools/conformance.py --engine native=target/debug/uplc-native --allow-unsupported

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
	$(PYTHON) tools/generate_cases.py --seed 42 --count 1000

import-plutus: references provenance
	$(PYTHON) tools/import_plutus.py
