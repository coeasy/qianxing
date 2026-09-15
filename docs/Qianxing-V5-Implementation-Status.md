# Qianxing V5 Implementation Status

## Phase 1 Started

## Goals

- Replace legacy crate organization with V5 architecture.
- Build kernel and domain foundations first.
- Remove compatibility layers.

## Execution Order

1. qx-kernel
2. qx-domain
3. qx-market
4. qx-trading
5. qx-runtime
6. ecosystem modules

## Current Focus

Kernel primitives:

- EventEnvelope
- EventId
- Clock abstraction
- Replay foundation
- Snapshot foundation

Domain primitives:

- Instrument
- Order
- Trade
- Account
- Position

