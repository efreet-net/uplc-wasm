#!/usr/bin/env python3
"""Generate reproducible closed UPLC programs for differential tests (no network)."""
import argparse
import json
from pathlib import Path
import random


def generate(seed, count):
    rng = random.Random(seed)
    boundaries = [0, 1, -1, 2**31, 2**32, 2**53 + 1, 2**64, -(2**64), 2**127, -(2**127)]
    for i in range(count):
        a, b = (rng.choice(boundaries) if rng.randrange(3) else rng.randrange(-2**256, 2**256) for _ in range(2))
        builtin, result = rng.choice([('addInteger', a + b), ('subtractInteger', a - b), ('multiplyInteger', a * b)])
        term = f'[[(builtin {builtin}) (con integer {a})] (con integer {b})]'
        for depth in range(rng.randrange(5)):
            term = f'[(lam x{depth} x{depth}) {term}]' if rng.randrange(2) else f'(force (delay {term}))'
        yield {'id': f'generated/{seed}/{i}', 'profile': 'profiles/plutus-v3-pv11.json',
               'program': {'format': 'uplc_text', 'source': f'(program 1.0.0 {term})'},
               'mode': {'kind': 'restricting', 'budget': {'cpu': '1000000000000', 'mem': '1000000000000'}},
               'expected': {'status': 'success', 'term': ['constant', ['integer', str(result)]], 'traces': []},
               'provenance': {'kind': 'generated', 'seed': seed, 'index': i}}


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--seed', type=int, default=42)
    parser.add_argument('--count', type=int, default=100)
    parser.add_argument('--output', type=Path, default=Path('.cache/generated.jsonl'))
    args = parser.parse_args()
    if args.count < 1:
        parser.error('count must be positive')
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(''.join(json.dumps(case, separators=(',', ':')) + '\n' for case in generate(args.seed, args.count)))
    print(f'{args.count} cases written with seed {args.seed}')
