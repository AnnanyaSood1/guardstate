// SPDX-License-Identifier: GPL-2.0
// Author: Annanya Sood <annanyas0142@gmail.com>

//! MIR-shaped intermediate representation consumed by the typestate analysis.
//!
//! This is deliberately a small, standalone model of the fragment of Rust MIR
//! that the atomic-context typestate lattice cares about: guard acquisition,
//! guard drop points (StorageDead / Drop), FFI calls, and control flow. It is
//! the Rust-side analogue of the `.c -> LLVM IR` inputs used by the C-side
//! Sleepability pass: no rustc, no kernel build, just enough IR to exercise O1.

use std::collections::BTreeMap;

/// A MIR local (e.g. the SSA-ish slot holding a lock guard).
pub type Local = String;

/// Basic-block identifier, e.g. "bb0".
pub type BbId = String;

/// How a lock guard relates to atomic (preemption-disabling) context.
///
/// Only preemption-disabling guards raise the lattice to AtomicHeld. Sleepable
/// locks (mutex, rw_semaphore) do NOT forbid sleeping, so their guards are
/// tracked but never raise context — matching the proposal's exclusion of
/// mutex guards from the atomic-context lattice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuardKind {
    /// spin_lock / raw_spin_lock / spin_lock_irqsave (non-PREEMPT_RT): raises context.
    PreemptDisabling,
    /// mutex_lock / down: sleepable, does not raise context.
    Sleepable,
}

impl GuardKind {
    pub fn from_acquire_name(name: &str) -> GuardKind {
        match name {
            "spin_lock" | "raw_spin_lock" | "spin_lock_irqsave" | "spin_lock_bh"
            | "local_irq_save" | "preempt_disable" => GuardKind::PreemptDisabling,
            _ => GuardKind::Sleepable, // mutex_lock, down, etc.
        }
    }
    pub fn raises_atomic(self) -> bool {
        matches!(self, GuardKind::PreemptDisabling)
    }
}

/// Straight-line statements inside a basic block.
#[derive(Debug, Clone)]
pub enum Stmt {
    /// `LOCAL = acquire KIND` — unconditional guard acquisition.
    Acquire { local: Local, kind: GuardKind },
    /// `drop LOCAL` — models the guard's Drop / StorageDead in post-drop-elaboration MIR.
    Drop { local: Local },
    /// `storage_dead LOCAL` — treated identically to Drop for guard liveness.
    StorageDead { local: Local },
}

/// Block terminators (control flow + calls, which in MIR are terminators).
#[derive(Debug, Clone)]
pub enum Term {
    /// `call SYMBOL -> BB` : an FFI / extern call site we may report on.
    Call { symbol: String, target: BbId },
    /// `call core::mem::drop(LOCAL) -> BB` : intrinsic drop, lowers the guard.
    MemDrop { local: Local, target: BbId },
    /// `call core::mem::forget(LOCAL) -> BB` : guard removed without drop;
    /// conservatively treated as STILL HELD (sound over-approximation).
    MemForget { local: Local, target: BbId },
    /// `try_acquire KIND into LOCAL -> success BB fail BB` : conditional acquire.
    /// The guard is live only on the success successor (path-sensitive trylock).
    TryAcquire { local: Local, kind: GuardKind, success: BbId, fail: BbId },
    /// `goto BB`
    Goto { target: BbId },
    /// `ret`
    Ret,
}

#[derive(Debug, Clone)]
pub struct Block {
    pub id: BbId,
    pub stmts: Vec<Stmt>,
    pub term: Term,
}

#[derive(Debug, Clone)]
pub struct Function {
    pub name: String,
    pub entry: BbId,
    pub blocks: BTreeMap<BbId, Block>,
    /// Source order of blocks, for deterministic iteration where needed.
    pub order: Vec<BbId>,
}

#[allow(dead_code)]
impl Function {
    pub fn block(&self, id: &str) -> Option<&Block> {
        self.blocks.get(id)
    }
}
