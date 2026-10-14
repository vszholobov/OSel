---
description: Why kernel globals use IrqSpinlock to mask interrupts and report deadlocks.
---

# 0003. IrqSpinlock for kernel globals

**Audience:** developers adding global kernel state.

**Summary:** `src/sync.rs` wraps globals in `IrqSpinlock`. Acquiring a lock disables interrupts; releasing it restores their previous state. Recursive acquisition reports the lock name and acquisition/wait locations instead of silently hanging.

**Status:** accepted. **Date:** 2026-10-12.

## Context

`spin::Mutex` with manually placed `without_interrupts` depended on discipline. A missing wrapper or processor exception such as `int3` during printing could hang the system without a report.

## Decision

`osel_core::sync::Spinlock` records a name and owner location and panics after `DEFAULT_MAX_SPINS`. Host tests include contention. `IrqSpinlock` saves, clears and restores the interrupt flag around it. All kernel globals use this wrapper; see [locking](../architecture/core/locking.md).

## Alternatives considered

Manual masking around `spin::Mutex` leaves one missed call capable of hanging the system. Panicking on `try_lock` failure is only suitable where contention is impossible and would fail with multiple CPUs.

## Consequences

Deadlocks produce readable panic reports. Exception handlers must avoid locks held by interrupted code and use `try_print`. Multi-CPU support will require revisiting the spin budget and lock order.
