# Qianxing Code Audit V2

## Scope

This document freezes the first audit baseline before implementation changes.

## Repository Baseline

Current repository is a Rust workspace. The visible top-level structure contains:

- `crates/`: Rust domain/runtime modules
- `cpp/`: native extension area
- `python/`: Python integration area
- `schemas/`: schema definitions
- `docs/`: architecture documents
- `tools/`: development tooling

## Current Architectural Assessment

### Strengths

1. Rust workspace provides a suitable foundation for deterministic trading infrastructure.
2. Existing separation between core, market/data, simulation, execution and plugin concepts matches the V5 direction.
3. Documentation already defines a deterministic event kernel direction.

### Problems To Fix

1. Domain contracts need to be frozen before adding more adapters.
2. Research/data abstractions and execution abstractions must not leak into kernel.
3. Plugin system must remain assembly-time oriented; runtime hot path must stay deterministic.
4. Cross-language interfaces need explicit schema/version boundaries.
5. Tests must evolve from module tests into end-to-end replay and invariant tests.

## Target Dependency Direction

```text
core/domain
    ↑
market/data adapters
    ↑
research/factor
    ↑
strategy
    ↑
portfolio/risk
    ↑
backtest/execution
    ↑
cli/control plane
```

## Refactor Rules

- Freeze event, time, identity, order, fill and ledger contracts.
- Keep external providers outside the kernel.
- Require reproducible manifests for every research/backtest run.
- Add compatibility tests before expanding modules.

## Next Implementation Stage

After architecture documents are synchronized, implementation proceeds with:

1. domain contract cleanup
2. unified runtime foundations
3. data/factor runtime
4. portfolio/risk runtime
5. event backtest expansion
