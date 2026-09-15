# Qianxing V5 Architecture Rebuild Plan

## Goal

Rebuild Qianxing as a Rust native quantitative infrastructure kernel.

The new architecture removes legacy module boundaries and does not keep compatibility layers.

## Target Modules

- qx-kernel
- qx-domain
- qx-market
- qx-trading
- qx-matching
- qx-accounting
- qx-risk
- qx-runtime
- qx-state
- qx-storage
- qx-extension
- qx-sdk
- qx-observability
- qx-cli

## Migration Rules

1. Freeze kernel boundaries.
2. Use Event Sourcing as the system state model.
3. Separate Command and Event models.
4. Unify Backtest, Paper and Live execution pipelines.
5. Keep finkit as an external factor computation engine.
6. Remove obsolete crates instead of adding compatibility adapters.

## Implementation Order

Phase 1: Workspace and dependency graph rebuild.

Phase 2: Kernel and domain model implementation.

Phase 3: Market, trading and matching engine reconstruction.

Phase 4: Runtime supervisor and state management.

Phase 5: SDK, plugin system and observability.

Phase 6: Full benchmark and end-to-end validation.
