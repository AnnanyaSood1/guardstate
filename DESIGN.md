<!--
SPDX-License-Identifier: GPL-2.0
Author: Annanya Sood <annanyas0142@gmail.com>
-->

# guardstate — Design Document

**Author:** Annanya Sood &lt;annanyas0142@gmail.com&gt;
**Component:** Rust-side atomic-context typestate analyzer (the Rust half of CLSC)
**Status:** research prototype; O1 algorithm + call-site join, on MIR-shaped input

---

## Table of contents

1. [Purpose and context](#1-purpose-and-context)
2. [Goals and non-goals](#2-goals-and-non-goals)
3. [Requirements](#3-requirements)
4. [Architecture overview](#4-architecture-overview)
5. [Data model: the MIR-shaped IR](#5-data-model-the-mir-shaped-ir)
6. [The analysis](#6-the-analysis)
7. [The call-site join](#7-the-call-site-join)
8. [Design decisions and rationale](#8-design-decisions-and-rationale)
9. [Soundness argument](#9-soundness-argument)
10. [Precision considerations](#10-precision-considerations)
11. [Testing strategy](#11-testing-strategy)
12. [Limitations and threats to validity](#12-limitations-and-threats-to-validity)
13. [Future work](#13-future-work)
14. [References](#14-references)

---

## 1. Purpose and context

In the Linux kernel, invoking a routine that **may sleep** (block or reschedule)
while in **atomic context** — holding a spinlock, in an interrupt handler, or in
an RCU read-side section — can deadlock or hang the machine. In Rust-for-Linux
this single safety contract is *split across two languages*:

- the **atomic context** is established on the **Rust** side by a live lock guard
  (e.g. a `SpinLockGuard`), while
- the **sleepability** of the invoked routine lives on the **C** side, reached
  through an `unsafe` FFI call — commonly a `rust_helper_*` shim wrapping a
  sleeping primitive such as `mutex_lock`, or a `GFP_KERNEL` allocation.

No existing analysis sees both halves: `klint` tracks Rust context but needs
manual annotation at the FFI boundary; `DSAC` finds sleep-in-atomic bugs in C but
cannot see the Rust caller or its guard. The violation lives in the seam — the
**split-invariant problem**.

**CLSC** (Cross-Language Sleepability Checker) closes the seam by computing each
half over its native IR and joining them at the FFI call site. This document
specifies the **Rust half and the join**: `guardstate`. The C half — the
sleepability summary over LLVM IR — is a companion component, consumed here as a
plain-text summary file.

This maps onto the research objectives as: **O1** (Rust-side atomic-context
typestate) fully, and **O3** (the call-site join) as a working teaser; **O2**
(the C-side summary) is external input.

---

## 2. Goals and non-goals

### Goals

- **G1.** Compute, for each unsafe FFI call site, whether a preemption-disabling
  guard is live (the atomic-context typestate), precisely across branches,
  drops, early returns, and conditional (`trylock`) acquisition.
- **G2.** Join that context with a C-side CanSleep summary and report exactly the
  boundary-spanning sleep-in-atomic violations.
- **G3.** Be a **sound detector** over the modelled fragment: never lose a guard
  the concrete execution still holds (no under-reporting of atomic context).
- **G4.** Be **deterministic and diff-testable**, so behaviour is pinned by
  golden outputs and regressions surface as diffs.
- **G5.** Be **buildable and reproducible** with no dependencies and no special
  toolchain, mirroring how the C-side pass is exercised on `.c`-derived IR.

### Non-goals (for this artifact)

- **N1.** Ingesting real `rustc` MIR. The front-end is a MIR-*shaped* text format;
  real MIR ingestion via `dylint` is deliberately deferred (see §13).
- **N2.** Resolving indirect / function-pointer calls, vtable dispatch, or the
  C→Rust callback path. These are the proposal's v2 frontier.
- **N3.** Full context sensitivity or a verifier-grade guarantee. CLSC is by
  design a heuristic detector: a sound over-approximation over the direct-call
  subgraph, no more.

Stating the non-goals precisely is itself a design goal: the artifact should
demonstrate that the O1 algorithm and the join are correct, not overclaim
completeness.

---

## 3. Requirements

**Functional.**

- FR1. Parse a function-structured, basic-block CFG with guard acquisition,
  drop / `StorageDead`, FFI calls, conditional acquire, and branches.
- FR2. Classify each acquire as preemption-disabling or sleepable.
- FR3. Produce a per-call-site verdict `{ATOMIC-HELD, SLEEPABLE}`.
- FR4. Given a CanSleep set, produce the violation set via the join predicate.

**Quality attributes.**

- QA1 (soundness-as-detector). The abstract context over-approximates the
  concrete: any guard held on any real path reaching a site is reflected there.
- QA2 (determinism). Output ordering is a total order on `(function, block)`.
- QA3 (reproducibility). No external dependencies; standard-library only.
- QA4 (legibility). Output is human-readable and mechanically diffable; the code
  favours obvious correctness over cleverness, appropriate to a research
  artifact meant to be read and audited.

---

## 4. Architecture overview

A three-stage pipeline, one Rust module per stage, plus the IR type definitions:

```
 input.mir ──▶ parse ──▶ Function CFG ──▶ analyze ──▶ CallSite verdicts ──▶ join ──▶ Violations
              (parse.rs)   (ir.rs)        (analysis.rs)                    (analysis.rs)
                                                    ▲
                              can_sleep.txt ────────┘  (C-side O2 summary)
```

| Module | Responsibility |
| --- | --- |
| `src/ir.rs` | IR types (`Function`, `Block`, `Stmt`, `Term`) and `GuardKind` classification. |
| `src/parse.rs` | Line-oriented parser: text → in-memory CFG. |
| `src/analysis.rs` | The typestate lattice, the fixpoint dataflow, and the join. |
| `src/main.rs` | CLI: read input, load summary, run analysis + join, print deterministically. |

The stages are decoupled so the front-end can later be swapped (text IR → real
MIR) without touching the analysis or the join.

---

## 5. Data model: the MIR-shaped IR

### Why MIR-shaped, not source or real MIR

The property depends on **execution semantics**, not source appearance: an early
`core::mem::drop(guard)`, the implicit drops inserted on `?` and early-return
paths, and lifetime subtleties all move a guard's true drop point away from its
lexical scope. Post-drop-elaboration MIR makes those drop points explicit, which
is why the eventual analysis targets MIR. For this prototype we model the *shape*
of that MIR — the minimum needed to exercise the lattice — so the algorithm can
be proven without the weight of `rustc` internals. This mirrors the C-side pass,
which runs on `.c` files lowered to LLVM IR rather than a full kernel build.

### Types (`src/ir.rs`)

- `Function { name, entry, blocks: BTreeMap<BbId, Block>, order }`
- `Block { id, stmts: Vec<Stmt>, term: Term }`
- `Stmt` ∈ { `Acquire{local, kind}`, `Drop{local}`, `StorageDead{local}` }
- `Term` ∈ { `Call{symbol, target}`, `MemDrop{local, target}`,
  `MemForget{local, target}`, `TryAcquire{local, kind, success, fail}`,
  `Goto{target}`, `Ret` }

Calls are terminators, as in real MIR.

### Guard-kind classification

`GuardKind::from_acquire_name` maps acquire symbols to two classes:

- **PreemptDisabling** (raises atomic context): `spin_lock`, `raw_spin_lock`,
  `spin_lock_irqsave`, `spin_lock_bh`, `local_irq_save`, `preempt_disable`.
- **Sleepable** (does not raise): everything else (`mutex_lock`, `down`, …).

This encodes the proposal's rule that, on non-`PREEMPT_RT` configurations, only
raw spinlocks and explicit preemption/IRQ-disabling guards establish atomic
context, while mutex guards — whose contract permits sleeping — are excluded.

---

## 6. The analysis

### Lattice

Two points, `Sleepable ⊑ AtomicHeld`. The abstract state at a program point is
the **set of live preemption-disabling guard locals**; the point is `AtomicHeld`
iff that set is non-empty. Join is set union.

### Transfer functions (working set `S`)

| Construct | Effect |
| --- | --- |
| `g = acquire K` | `S ∪ {g}` if `K` preempt-disabling, else `S` |
| `drop g` / `storage_dead g` | `S \ {g}` |
| `call sym -> t` | record site `(sym, AtomicHeld = S≠∅)`; propagate `S` |
| `drop_call g -> t` (`mem::drop`) | propagate `S \ {g}` |
| `forget_call g -> t` (`mem::forget`) | propagate `S` **unchanged** (conservative) |
| `try g = K -> succ fail` | `succ` gets `S ∪ {g}` (if `K` raises); `fail` gets `S` |
| `goto t` | propagate `S` |
| `ret` | no successor |

### Fixpoint

A forward dataflow to a least fixpoint. Block in-states start empty; each round
recomputes every block's contribution to its successors and unions them into
successor in-states, with the entry block pinned empty. Iteration halts when no
in-state changes. Merges take the **union** (may-held) of predecessor states.

### Complexity

For a function with `B` blocks and `G` distinct guards, a state is a subset of
guards, so the lattice height is `O(G)` and the round-robin fixpoint converges in
`O(B · G)` iterations, each `O(B · G)` work — polynomial and, for realistic
per-function CFGs, effectively linear. This keeps the per-function cost within the
FiTx-style development-time budget the larger project targets.

---

## 7. The call-site join

The C-side summary is a set of CanSleep callee symbols (one per line). The join
implements the O3 predicate literally:

```
report(site)  ⇔  State(site) = AtomicHeld  ∧  callee(site) ∈ CanSleep
```

Violations are sorted by `(function, block)` for deterministic output. The
summary file is the contract between the two halves of CLSC; nothing else about
the C-side pass leaks into `guardstate`.

---

## 8. Design decisions and rationale

**D1 — Standalone text IR before real MIR.** *Alternatives:* build directly as a
`rustc` driver or a `dylint` lint. *Decision:* start standalone. *Why:* real MIR
ingestion needs `rustc_private` and a pinned nightly, is version-fragile, and
would entangle the algorithm's correctness with toolchain plumbing. Proving the
lattice on MIR-shaped inputs first de-risks O1 cheaply and keeps the artifact
reproducible; the front-end is isolated so the swap to `dylint` is local.

**D2 — Set of live guards, not a single boolean.** *Alternatives:* a single
`AtomicHeld` bit or a counter. *Decision:* track the set of live guard locals.
*Why:* nested guards need precise release — dropping one guard while another is
held must remain `AtomicHeld`. A bit cannot express "which" guard dropped; a
counter mishandles double-count/idempotent cases. The set is exact and cheap.

**D3 — Union (may-held) at merges.** *Alternatives:* intersection (must-held).
*Decision:* union. *Why:* for a *detector* the sound direction is to over-
approximate context — a guard held on any incoming path is treated as live, so we
never miss a violation. Intersection would drop guards held on only some paths
and silently under-report. (A future *verifier* mode might add a must-analysis to
distinguish definite from possible violations.)

**D4 — `mem::forget` keeps the guard held.** *Alternatives:* treat `forget` as a
release. *Decision:* keep it. *Why:* `forget` suppresses the guard's `Drop`, so
the lock is never released and preemption stays disabled; the concrete context
remains atomic. Keeping the guard is the sound choice and matches the proposal.

**D5 — Path-sensitive `trylock`.** *Alternatives:* treat `spin_trylock` as an
unconditional acquire. *Decision:* raise context only on the success successor.
*Why:* a failed `try_lock` holds no lock; raising context on the failure path
would manufacture false positives. Splitting the successors keeps both branches
exact and directly addresses a limitation `klint`'s author notes.

**D6 — Deterministic, sorted, diffable output.** *Alternatives:* report in
traversal order. *Decision:* total order on `(function, block)`. *Why:* golden-
file testing and CI require stable output; a diff is then a precise regression
signal. This mirrors the C-side pass's frozen-expected discipline.

**D7 — Zero dependencies.** *Decision:* standard library only. *Why:* offline,
deterministic builds and a trivially auditable artifact; nothing to pin or vendor.

---

## 9. Soundness argument

We argue **no missed violation on the modelled fragment**, i.e. the abstract
atomic context over-approximates the concrete (the C side separately over-
approximates sleepability, so the conjunction is a sound detector).

- **Merges union.** If a guard is held on some concrete path reaching a site, it
  appears in the abstract in-state there (union never drops a predecessor's
  guard). Hence `AtomicHeld` is never under-reported at a merge.
- **Acquire/drop are exact on the fragment.** Acquire inserts, drop/`StorageDead`
  removes, keyed on the elaborated drop point rather than lexical scope; so `?`,
  early return, and explicit `drop` are handled by construction.
- **`mem::forget` conservative.** As in D4, keeping the guard matches the
  concrete non-release; never unsound.
- **`trylock` split.** The success path holds the lock and the failure path does
  not; modelling both exactly is sound and precise.
- **Unknown callees / flags** are the C side's responsibility, where an
  unresolved GFP flag is treated as sleeping and the summary is a conservative
  may-sleep set.

Soundness is claimed only over this fragment and the direct-call subgraph,
consistent with CLSC's stated posture as a heuristic detector, not a verifier.

---

## 10. Precision considerations

Soundness (no missed bugs) is cheap; the engineering challenge in the larger
project is **precision** (few false positives), because transitive sleepability
over-approximates. `guardstate` contributes precision on the Rust side by:

- excluding sleepable-lock guards (D2/classification), so mutex sections are not
  spuriously flagged;
- the `trylock` success/fail split (D5), so failed acquisitions do not raise
  context;
- exact drop points, so a guard released before a call clears context.

Deliberately **out of scope but named** for the join's precision: recognising
in-band guards such as `in_atomic()` checks, and path pruning — both to be added
only where the fault-injection controls show a measurable false-positive drop, as
the evaluation plan prescribes. Learned/opaque filters are avoided by design, as
kernel maintainers rightly distrust them.

---

## 11. Testing strategy

**Golden-file, control-per-behaviour.** Seven kernel-shaped inputs, each
isolating one behaviour; each diffed against a frozen expected output by
`run_tests.sh`. The suite pairs positive controls (must fire) with near-miss
negative controls (must not), which is what distinguishes a real analysis from
one that simply flags everything.

| Test | Behaviour under test | Polarity |
| --- | --- | --- |
| `t1_spinlock_sleep` | canonical spinlock + sleeping callee | positive |
| `t2_trylock_pathsens` | `trylock` success vs. fail | mixed |
| `t3_early_drop` | drop before call | negative |
| `t4_mutex_guard` | sleepable-lock guard | negative |
| `t5_nonsleeping_callee` | held guard, non-sleeping callee | negative (precision) |
| `t6_forget` | `mem::forget` keeps context | positive (soundness) |
| `t7_early_return` | `?`-shape: held vs. dropped path | mixed |

This is the Rust-side analogue of the C-side pass's control inputs, and the two
suites together characterise the join.

---

## 12. Limitations and threats to validity

- **Model fidelity.** Results are on MIR-shaped inputs, not real `rustc` MIR; the
  chief threat is that hand-written IR does not capture a construct real MIR would
  produce. Mitigation: the `dylint` step (§13) replaces the front-end with real
  MIR while keeping the analysis and tests.
- **Direct calls only.** Indirect/vtable dispatch and C→Rust callbacks are
  unmodelled; coverage, not soundness, is affected on the modelled fragment.
- **Summary provenance.** The join is only as good as the C-side CanSleep set;
  its precision (argument-sensitive GFP handling) is the companion component's
  concern.
- **Configuration assumptions.** Guard classification assumes non-`PREEMPT_RT`
  semantics; under `PREEMPT_RT`, `spinlock_t` becomes sleeping and the
  classification table would need adjustment.

---

## 13. Future work

In priority order, mapping to the project plan:

1. **Real MIR via `dylint` (O1 → P2).** Swap the text front-end for a `dylint`
   lint reading post-drop-elaboration MIR from a real Rust-for-Linux driver;
   keep this lattice and join unchanged. Highest-value next step.
2. **Real C-side summary (O2 integration).** Emit `can_sleep.txt` from the LLVM
   Sleepability pass on a linked kernel slice and run a genuine cross-IR check.
3. **Precision controls.** Add a nested two-guard case and in-band `in_atomic()`
   recognition, tuned against fault-injection controls.
4. **Beyond v1.** Indirect/function-pointer resolution (connection-based alias
   analysis on the C side; callsite-sensitive MIR pointer analysis on the Rust
   side) and the C→Rust callback mirror invariant.

---

## 14. References

- J.-J. Bai, J. Lawall, S.-M. Hu. *Effective detection of sleep-in-atomic-context
  bugs in the Linux kernel.* ACM TOCS 36(4), 2020. (DSAC.)
- J. Corbet. *Preventing atomic-context violations in Rust code with klint.*
  LWN.net, 2023. (klint.)
- K. Suzuki, K. Ishiguro, K. Kono. *Balancing analysis time and bug detection:
  daily development-friendly bug detection in Linux.* USENIX ATC, 2024. (FiTx.)
- T. Li, J.-J. Bai, Y. Sui, S.-M. Hu. *Path-sensitive and alias-aware typestate
  analysis for detecting OS bugs.* ASPLOS, 2022.
- R. E. Strom, S. Yemini. *Typestate: a programming language concept for
  enhancing software reliability.* IEEE TSE, 1986.
- Y. Sui, J. Xue. *SVF: interprocedural static value-flow analysis in LLVM.* CC,
  2016.
- C. Lattner, V. Adve. *LLVM: a compilation framework for lifelong program
  analysis & transformation.* CGO, 2004.

See the research proposal for the complete bibliography and the full CLSC design.
