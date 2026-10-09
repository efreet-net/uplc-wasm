#!/usr/bin/env python3
"""Generate reproducible arithmetic UPLC, optionally dual-encoded into raw Flat."""
import argparse
import json
from pathlib import Path
import random


def generate(seed, count, division=False):
    """Keep mathematical expectations independent of all UPLC evaluators."""
    rng = random.Random(seed)
    boundaries = [0, 1, -1, 2**31, 2**32, 2**53 + 1, 2**64, -(2**64), 2**127, -(2**127)]
    for i in range(count):
        a, b = (rng.choice(boundaries) if rng.randrange(3) else rng.randrange(-2**256, 2**256) for _ in range(2))
        choices = [('addInteger', a + b), ('subtractInteger', a - b), ('multiplyInteger', a * b)]
        if division:
            from build_division_corpus import DIVISION, division_result
            choices += [(name, division_result(name, a, b) if b else None) for name in DIVISION]
        builtin, result = rng.choice(choices)
        term = f'[[(builtin {builtin}) (con integer {a})] (con integer {b})]'
        wrappers = []
        for depth in range(rng.randrange(5)):
            if rng.randrange(2):
                term = f'[(lam x{depth} x{depth}) {term}]'
                wrappers.append('identity')
            else:
                term = f'(force (delay {term}))'
                wrappers.append('force-delay')
        expected = ({'status': 'success', 'term': ['constant', ['integer', str(result)]], 'traces': []}
                    if result is not None else {'status': 'failure', 'kind': 'evaluation', 'traces': []})
        yield {'id': f'generated{"-division" if division else ""}/{seed}/{i}', 'profile': 'profiles/plutus-v3-pv11.json',
               'program': {'format': 'uplc_text', 'source': f'(program 1.0.0 {term})'},
               'mode': {'kind': 'restricting', 'budget': {'cpu': '1000000000000', 'mem': '1000000000000'}},
               'expected': expected,
               'provenance': {'kind': 'generated', 'seed': seed, 'index': i,
                              'arithmetic': {'builtin': builtin, 'arguments': [str(a), str(b)],
                                             'wrappers_inner_to_outer': wrappers}}}


def execution_ledger(arithmetic):
    failed = arithmetic['builtin'] in ('divideInteger', 'quotientInteger', 'remainderInteger', 'modInteger') and arithmetic['arguments'][1] == '0'
    events = ['apply', 'apply', 'builtin', 'constant', 'constant',
              {'builtin': arithmetic['builtin'], 'arguments': arithmetic['arguments']}]
    for wrapper in arithmetic['wrappers_inner_to_outer']:
        if wrapper == 'identity':
            events = ['apply', 'lambda'] + events + ([] if failed else ['var'])
        elif wrapper == 'force-delay':
            events = ['force', 'delay'] + events
        else:
            raise ValueError('unknown generated wrapper')
    return events + ['error' if failed else 'halt']


def generate_flat(seed, count, pins, sources, encoders, root=None, division=False):
    # Shared provenance helpers invoke encoding only. There is no textual parser
    # or evaluation mode hidden in this generator.
    if division:
        from build_division_corpus import SPEC_SOURCES, ledger_outcome
    else:
        from build_builtin_corpus import SPEC_SOURCES, ledger_outcome
    from build_milestone_corpus import PROFILE, attach_flat, raw_record, sha256
    from conformance import ROOT
    root = ROOT if root is None else root
    profile = json.loads((root / PROFILE).read_text())
    anchors = {path: sha256((sources['plutus'] / path).read_bytes()) for path in SPEC_SOURCES}
    generator_sha = sha256((root / 'tools/generate_cases.py').read_bytes())
    for case in generate(seed, count, division=division):
        provenance = case['provenance']
        events = execution_ledger(provenance['arithmetic'])
        original_term = case['expected'].get('term')
        case['expected'] = ledger_outcome(events, profile['cost_model']['parameters'],
                                          case['mode']['budget'], original_term)
        provenance.update(kind='generated-division-derived-flat' if division else 'generated-arithmetic-derived-flat',
                          source=raw_record(f'tools/generate_cases.py#seed={seed},index={provenance["index"]}',
                                            case['program']['source'].encode()),
                          generator_sha256=generator_sha, events=events,
                          cost_model_sha256=profile['cost_model']['sha256'],
                          plutus_revision=pins['plutus']['revision'], spec_sources=anchors)
        attach_flat(case, pins, encoders)
        yield case


def main(args):
    from build_milestone_corpus import encode_jsonl, publish, verify_profile
    from conformance import Engine, ROOT
    from upstreams import verify_source
    if args.flat:
        pins = json.loads((ROOT / 'upstreams.lock.json').read_text())
        sources = {name: verify_source(name, pin) for name, pin in pins.items()}
        verify_profile(json.loads((ROOT / 'profiles/plutus-v3-pv11.json').read_text()), sources['aiken'])
        encoders = [Engine(name + '-flat-encoder', command + ' --encode-flat', args.timeout)
                    for name, command in (('aiken', args.aiken), ('amaru', args.amaru))]
        try:
            data = encode_jsonl(generate_flat(args.seed, args.count, pins, sources, encoders, division=args.division))
        finally:
            for encoder in encoders:
                encoder.close()
    else:
        data = encode_jsonl(generate(args.seed, args.count, division=args.division))
    if args.check:
        if args.output.read_bytes() != data:
            raise ValueError('generated corpus differs from independent reconstruction')
    else:
        publish(args.output, data)
    print(f'{args.count} {"Flat" if args.flat else "text"} cases {"checked" if args.check else "written"} with seed {args.seed}')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--seed', type=int, default=42)
    parser.add_argument('--count', type=int, default=100)
    parser.add_argument('--output', type=Path, default=Path('.cache/generated.jsonl'))
    parser.add_argument('--division', action='store_true', help='extend seeded arithmetic with exact signed division and zero-divisor failure ledgers')
    parser.add_argument('--flat', action='store_true', help='require the two pinned reference encoders to agree on raw Flat')
    parser.add_argument('--aiken', default='tools/oracle-aiken/target/debug/oracle-aiken')
    parser.add_argument('--amaru', default='tools/oracle-amaru/target/debug/oracle-amaru')
    parser.add_argument('--timeout', type=float, default=10)
    parser.add_argument('--check', action='store_true', help='reconstruct without overwriting existing output')
    args = parser.parse_args()
    if args.count < 1:
        parser.error('count must be positive')
    main(args)
