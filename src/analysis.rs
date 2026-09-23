// SPDX-License-Identifier: GPL-2.0
// Author: Annanya Sood <annanyas0142@gmail.com>

//! Atomic-context typestate analysis (Rust-side half of CLSC, objective O1)
//! plus the call-site join with a C-side sleepability summary (O3 teaser).
//!
//! Lattice: two points, Sleepable ⊑ AtomicHeld, ordered by whether any
//! preemption-disabling guard is live. We represent the abstract state at a
//! program point as the SET of live atomic-raising guard locals; the state is
//! AtomicHeld iff that set is non-empty. Using the set (rather than a single
//! bit) makes drops precise when several guards nest.
//!
//! The transfer is a forward, path-sensitive dataflow to a least fixpoint over
//! the block CFG. At control-flow merges we take the UNION of predecessor
//! states (a guard held on any incoming path is considered live), which is the
//! sound direction for a bug *detector*: we would rather keep a guard than lose
//! one and miss a violation. `spin_trylock` is handled path-sensitively — the
//! guard becomes live only on the success successor.

use crate::ir::*;
use std::collections::{BTreeMap, BTreeSet};

/// Set of live preemption-disabling guard locals at a program point.
type State = BTreeSet<Local>;

/// A recorded FFI call site and the context in which it executes.
#[derive(Debug, Clone)]
pub struct CallSite {
    pub func: String,
    pub bb: BbId,
    pub symbol: String,
    pub atomic_held: bool,
}

/// Kind classification for each acquired local, gathered up front so drops of
/// sleepable guards are ignored for the atomic bit.
fn collect_guard_kinds(f: &Function) -> BTreeMap<Local, GuardKind> {
    let mut kinds = BTreeMap::new();
    for id in &f.order {
        let b = &f.blocks[id];
        for s in &b.stmts {
            if let Stmt::Acquire { local, kind } = s {
                kinds.insert(local.clone(), *kind);
            }
        }
        if let Term::TryAcquire { local, kind, .. } = &b.term {
            kinds.insert(local.clone(), *kind);
        }
    }
    kinds
}

/// Apply a block's straight-line statements to `st`, mutating it in place.
fn apply_stmts(b: &Block, st: &mut State) {
    for s in &b.stmts {
        match s {
            Stmt::Acquire { local, kind } => {
                if kind.raises_atomic() {
                    st.insert(local.clone());
                }
            }
            Stmt::Drop { local } | Stmt::StorageDead { local } => {
                st.remove(local);
            }
        }
    }
}

/// Run the forward fixpoint. Returns the in-state (state on entry) of each block.
fn fixpoint(f: &Function) -> BTreeMap<BbId, State> {
    // Predecessor edges, with the state each edge propagates computed lazily.
    let mut in_state: BTreeMap<BbId, State> = f.order.iter().map(|id| (id.clone(), State::new())).collect();

    // Iterate to fixpoint. CFGs here are tiny; a simple round-robin is plenty
    // and keeps the code obviously correct rather than clever.
    let mut changed = true;
    while changed {
        changed = false;
        // out-state contributions from each block to its successors
        let mut incoming: BTreeMap<BbId, State> = f.order.iter().map(|id| (id.clone(), State::new())).collect();

        for id in &f.order {
            let b = &f.blocks[id];
            let mut st = in_state[id].clone();
            apply_stmts(b, &mut st);
            // Distribute to successors per the terminator's semantics.
            let push = |target: &BbId, s: &State, incoming: &mut BTreeMap<BbId, State>| {
                if let Some(dst) = incoming.get_mut(target) {
                    for g in s {
                        dst.insert(g.clone());
                    }
                }
            };
            match &b.term {
                Term::Call { target, .. } => push(target, &st, &mut incoming),
                Term::MemDrop { local, target } => {
                    let mut s2 = st.clone();
                    s2.remove(local); // core::mem::drop lowers the guard
                    push(target, &s2, &mut incoming);
                }
                Term::MemForget { target, .. } => {
                    // Conservative: forget does NOT drop the guard for our purposes.
                    push(target, &st, &mut incoming);
                }
                Term::TryAcquire { local, kind, success, fail } => {
                    let mut succ = st.clone();
                    if kind.raises_atomic() {
                        succ.insert(local.clone()); // live only on success
                    }
                    push(success, &succ, &mut incoming);
                    push(fail, &st, &mut incoming);
                }
                Term::Goto { target } => push(target, &st, &mut incoming),
                Term::Ret => {}
            }
        }

        // Entry block always starts empty; merge incoming into in_state.
        for id in &f.order {
            let mut merged = incoming[id].clone();
            if *id == f.entry {
                // entry has no predecessors; keep empty
                merged = State::new();
            }
            if merged != in_state[id] {
                in_state.insert(id.clone(), merged);
                changed = true;
            }
        }
    }
    in_state
}

/// Analyze one function: return every FFI call site with its atomic context.
pub fn analyze_fn(f: &Function) -> Vec<CallSite> {
    let _kinds = collect_guard_kinds(f); // (kept for clarity / future kind-aware reporting)
    let in_state = fixpoint(f);
    let mut sites = Vec::new();

    for id in &f.order {
        let b = &f.blocks[id];
        let mut st = in_state[id].clone();
        apply_stmts(b, &mut st);
        if let Term::Call { symbol, .. } = &b.term {
            sites.push(CallSite {
                func: f.name.clone(),
                bb: id.clone(),
                symbol: symbol.clone(),
                atomic_held: !st.is_empty(),
            });
        }
    }
    // Deterministic ordering for diffable output.
    sites.sort_by(|a, b| (a.func.as_str(), a.bb.as_str()).cmp(&(b.func.as_str(), b.bb.as_str())));
    sites
}

/// A reported split-invariant violation.
#[derive(Debug, Clone)]
pub struct Violation {
    pub func: String,
    pub bb: BbId,
    pub symbol: String,
}

/// The call-site join: report exactly when the site is AtomicHeld AND the
/// callee is CanSleep in the C-side summary. This is the O3 predicate
/// `State(site) = AtomicHeld ∧ Summary(callee) = CanSleep`.
pub fn join(sites: &[CallSite], can_sleep: &BTreeSet<String>) -> Vec<Violation> {
    let mut v: Vec<Violation> = sites
        .iter()
        .filter(|s| s.atomic_held && can_sleep.contains(&s.symbol))
        .map(|s| Violation { func: s.func.clone(), bb: s.bb.clone(), symbol: s.symbol.clone() })
        .collect();
    v.sort_by(|a, b| (a.func.as_str(), a.bb.as_str()).cmp(&(b.func.as_str(), b.bb.as_str())));
    v
}
