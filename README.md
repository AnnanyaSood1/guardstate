![CI](https://github.com/AnnanyaSood1/guardstate/actions/workflows/ci.yml/badge.svg)
# guardstate

**A Rust-side atomic-context typestate analyzer for detecting sleep-in-atomic
violations at the Rust→C FFI boundary — the Rust half of CLSC.**

`guardstate` computes, for a MIR-shaped input program, whether a
preemption-disabling lock guard is live at each unsafe FFI call site (the
*atomic-context typestate*), and — given a C-side sleepability summary — joins
the two to report **sleep-in-atomic violations** that span the language
boundary. It is the companion to the C-side LLVM *Sleepability* pass: one decides
context over Rust MIR, the other decides sleepability over C LLVM IR, and the
join decides the boundary-spanning bug that neither half can see alone.

> Status: research prototype. Standalone, dependency-free (Rust std only).
> 7/7 tests passing. Models the O1 algorithm on MIR-shaped inputs; does not yet
> ingest real `rustc` MIR (see [Roadmap](#roadmap)).

---

**Successor project.** A workspace-refactored generalization that runs the same lattice over real rustc MIR is in active development at guardstate-mir. This repository remains the completed, self-contained proof of the analysis logic.

## Table of contents

- [Motivation: the split-invariant problem](#motivation-the-split-invariant-problem)
- [Where guardstate fits in CLSC](#where-guardstate-fits-in-clsc)
- [Scope: what it is and is not](#scope-what-it-is-and-is-not)
- [Quick start](#quick-start)
- [The input IR](#the-input-ir)
- [How it works](#how-it-works)
- [A worked example](#a-worked-example)
- [The test suite](#the-test-suite)
- [Output format](#output-format)
- [Project layout](#project-layout)
- [Reproducibility](#reproducibility)
- [Roadmap](#roadmap)
- [Relationship to the C-side artifact](#relationship-to-the-c-side-artifact)
- [References](#references)
- [Authorship, license, and citation](#authorship-license-and-citation)

---

## Motivation: the split-invariant problem

In the Linux kernel, calling a function that **may sleep** (block or reschedule)
while in **atomic context** — holding a spinlock, in an interrupt handler, inside
an RCU read-side section — can hang the machine. In Rust-for-Linux this one
safety contract is split across two languages:

- the **atomic context** is established on the **Rust** side by a live lock guard
  (e.g. a `SpinLockGuard`), and
- the **sleepability** of the routine being called lives on the **C** side,
  reached through an `unsafe` FFI call — often a `rust_helper_*` shim wrapping a
  sleeping primitive such as `mutex_lock`, or a `GFP_KERNEL` allocation.

Existing analyses see only one half. `klint` tracks atomic context in Rust but
requires manual annotation at the FFI boundary. `DSAC` finds sleep-in-atomic
bugs in C but never sees the Rust caller or its guard. The violation lives in the
seam between them — the **split-invariant problem**. `guardstate` implements the
Rust half and the seam.

---

## Where guardstate fits in CLSC

CLSC (Cross-Language Sleepability Checker) decides the split invariant by
computing each half over its native IR and joining them at the FFI call site.
This repository is the Rust half plus the join:

| CLSC objective | Component | This repo |
| --- | --- | --- |
| **O1** — Rust-side atomic-context typestate over MIR | `guardstate` typestate analysis | ✅ implemented (on MIR-shaped input) |
| **O2** — C-side sleepability summary over LLVM IR | LLVM *Sleepability* pass | companion repo (consumed here as a summary file) |
| **O3** — the call-site join | `guardstate` join | ✅ teaser: `State(site)=AtomicHeld ∧ callee∈CanSleep` |

The join predicate is implemented literally:

```
report(site)  ⇔  State(site) = AtomicHeld  ∧  callee(site) ∈ CanSleep
```

where `State(site)` comes from this analyzer and `CanSleep` is the set emitted by
the C-side pass.

---

## Scope: what it is and is not

**It is** a correct, buildable implementation of the guard-liveness typestate
lattice and the call-site join, proven on **MIR-shaped inputs** — deliberately
symmetric to how the C-side pass is proven on kernel-shaped `.c` files lowered to
LLVM IR. No `rustc`, no kernel tree, no toolchain patching.

**It is not** (yet):

- an ingestor of real post-drop-elaboration MIR from `rustc` — that is the
  `dylint` / driver step (P2 of the plan);
- a resolver of indirect / function-pointer calls, or the C→Rust callback path
  (v2 in the plan);
- a verifier. CLSC is by design a **heuristic detector**: a sound
  over-approximation of atomic context over the modelled fragment and the
  direct-call subgraph, no more.

This honesty is the point: the artifact demonstrates that the O1 algorithm and
the join are right, not that CLSC as a whole is complete.

---

## Quick start

Requirements: a Rust toolchain (`rustc` + `cargo`, 1.75+). No other dependencies.

```sh
# Build
cargo build

# Run on a single test, with the C-side summary, to get violations
./target/debug/guardstate tests_mir/t1_spinlock_sleep.mir --summary tests_mir/can_sleep.txt

# Run the whole test suite (builds, runs each test, diffs against expected)
./run_tests.sh
```

Without `--summary`, the tool prints only the per-site context verdicts (useful
for inspecting the typestate in isolation); with it, it also prints the join and
the violations.

---

## The input IR

The input is a small, line-oriented, MIR-shaped text format. It models exactly
the fragment of Rust MIR the lattice cares about: guard acquisition, guard drop
points, FFI calls, and control flow. `#` starts a comment; whitespace is
insignificant.

### Grammar

| Form | Meaning |
| --- | --- |
| `fn NAME` | begins a function; the first `bb` is its entry |
| `bbN:` | begins a basic block |
| `LOCAL = acquire KIND` | unconditional guard acquire (statement) |
| `drop LOCAL` | guard drop / `StorageDead` (statement) |
| `storage_dead LOCAL` | identical to `drop` for guard liveness |
| `call SYMBOL -> bbN` | FFI/extern call terminator (a reportable call site) |
| `drop_call LOCAL -> bbN` | `core::mem::drop(LOCAL)` terminator — lowers the guard |
| `forget_call LOCAL -> bbN` | `core::mem::forget(LOCAL)` — guard kept (conservative) |
| `try LOCAL = KIND -> bbSucc bbFail` | conditional acquire (`spin_trylock`); guard live only on success |
| `goto bbN` | unconditional branch |
| `ret` | return |

### Guard kinds

`KIND` is a kernel acquire name. It is classified by whether it disables
preemption (and thus establishes atomic context):

- **Preemption-disabling** (raises context): `spin_lock`, `raw_spin_lock`,
  `spin_lock_irqsave`, `spin_lock_bh`, `local_irq_save`, `preempt_disable`.
- **Sleepable** (does *not* raise context): everything else, e.g. `mutex_lock`,
  `down`. Their guards permit sleeping, so they are tracked but never raise the
  lattice.

This mirrors the proposal's rule that on non-`PREEMPT_RT` configurations only
raw spinlocks and explicit preemption/IRQ-disabling guards qualify, while mutex
guards are excluded.

### Example

```
fn driver_write
bb0:
  g0 = acquire spin_lock          # atomic context begins
  call rust_helper_mutex_lock -> bb1
bb1:
  drop g0                         # atomic context ends
  ret
```

---

## How it works

Three stages, one per source module.

### 1. Parse (`src/parse.rs`)

The line-oriented parser turns the text into an in-memory CFG: a list of
`Function`s, each a map of `BbId → Block`, each block a list of `Stmt`s and one
terminating `Term`. The grammar is intentionally tiny so the parser is obviously
correct rather than clever.

### 2. Typestate dataflow (`src/analysis.rs`)

The lattice is two points, `Sleepable ⊑ AtomicHeld`. The abstract state at a
program point is the **set of live preemption-disabling guard locals**; the state
is `AtomicHeld` iff that set is non-empty. Using the set (not a single bit) makes
drops precise when guards nest — dropping one while another is held correctly
stays `AtomicHeld`.

**Transfer functions** (working set `S`):

- `g = acquire K` → if `K` is preemption-disabling, `S ∪ {g}`; else unchanged.
- `drop g` / `storage_dead g` → `S \ {g}`, keyed on the drop / `StorageDead`
  point, *not* lexical scope.
- `call sym -> t` → record site `(sym, AtomicHeld = S ≠ ∅)`; propagate `S` to `t`.
- `drop_call g -> t` (`core::mem::drop`) → propagate `S \ {g}`.
- `forget_call g -> t` (`core::mem::forget`) → propagate `S` **unchanged**
  (conservative: preemption stays disabled because the guard's `Drop` never runs).
- `try g = K -> succ fail` → propagate `S ∪ {g}` to `succ` (if `K` raises), `S`
  to `fail`. This is path-sensitive `spin_trylock`: the guard is live only where
  the acquire succeeded.
- `goto t` → propagate `S`. `ret` → no successor.

**Fixpoint.** A forward dataflow to a least fixpoint: block in-states start
empty, each round unions predecessor contributions into successor in-states (the
entry pinned empty), stopping when nothing changes. The **union at merges** is
the may-held direction — a guard held on any incoming path is considered live —
which is the sound direction for a detector.

See [`DESIGN.md`](DESIGN.md) for the full soundness argument.

### 3. The join (`src/analysis.rs` → `join`)

The C-side summary is loaded as a set of CanSleep callee symbols. A violation is
reported exactly when a call site is `AtomicHeld` **and** its callee is in that
set. Output is sorted by `(function, block)` for deterministic, diffable results.

---

## A worked example

Take `t7_early_return.mir`, an early-return (`?`) shape where a guard is dropped
on the error path but held on the main path:

```
fn driver_qmark
bb0:
  g0 = acquire spin_lock
  goto bb_check
bb_check:
  try t0 = spin_lock -> bb_main bb_err
bb_err:
  drop g0
  call rust_helper_mutex_lock -> bb_ret
bb_main:
  call rust_helper_mutex_lock -> bb_ret
bb_ret:
  ret
```

- On the path to `bb_main`, `g0` (from `bb0`) is still live and the `trylock`
  added `t0` — the set is `{g0, t0}`, so the call is **ATOMIC-HELD**.
- On the path to `bb_err`, the `trylock` failed (so no `t0`) and `g0` is dropped
  before the call — the set is empty, so the call is **SLEEPABLE**.

Output:

```
== atomic-context at FFI call sites ==
driver_qmark::bb_err -> rust_helper_mutex_lock       [SLEEPABLE]
driver_qmark::bb_main -> rust_helper_mutex_lock       [ATOMIC-HELD]

== sleep-in-atomic violations (State=AtomicHeld & callee=CanSleep) ==
VIOLATION driver_qmark::bb_main calls rust_helper_mutex_lock while holding atomic context

summary: 2 call site(s), 1 violation(s)
```

The analyzer distinguishes the two paths and fires only on the one that is truly
atomic — path-sensitivity and drop-point precision working together.

---

## The test suite

Seven kernel-shaped inputs, each isolating one behaviour of the lattice. They are
the Rust-side analogue of the C-side pass's control inputs.

| Test | Shape | Expected | What it proves |
| --- | --- | --- | --- |
| `t1_spinlock_sleep` | spinlock held, call sleeping `mutex_lock` | **violation** | the canonical detection case |
| `t2_trylock_pathsens` | `spin_trylock`, success vs. fail branch | violation on success only | path-sensitive conditional acquire |
| `t3_early_drop` | guard `mem::drop`-ped before the call | no violation | drop-point precision |
| `t4_mutex_guard` | mutex (sleepable) guard held | no violation | sleepable locks don't raise context |
| `t5_nonsleeping_callee` | spinlock held, non-sleeping callee | no violation | precision: near-miss control |
| `t6_forget` | `mem::forget(guard)` then sleeper | **violation** | conservative soundness |
| `t7_early_return` | early-return (`?`) shape | held on main path, clean on dropped path | path-sensitivity + drop precision together |

Run them with `./run_tests.sh`; each is diffed against a frozen expected output
in `tests_mir/expected/`.

---

## Output format

Two sections, both deterministically ordered:

1. **Atomic-context at FFI call sites** — one line per call site:
   `FUNCTION::BLOCK -> SYMBOL [ATOMIC-HELD | SLEEPABLE]`.
2. **Sleep-in-atomic violations** (only with `--summary`) — one line per
   violation, plus a `summary: N call site(s), M violation(s)` tally.

The frozen, sorted output is designed to be diffed in CI against an expected
file, so a regression shows up as a diff rather than a silent behaviour change.

---

## Project layout

```
guardstate/
├── Cargo.toml            # package manifest (author, metadata, no dependencies)
├── Cargo.lock
├── LICENSE               # GPL-2.0
├── README.md             # this file
├── DESIGN.md             # full design document (architecture, rationale, soundness)
├── run_tests.sh          # build + run all tests + diff against expected
├── src/
│   ├── main.rs           # CLI driver: parse, load summary, analyze, join, print
│   ├── ir.rs             # MIR-shaped IR types + guard-kind classification
│   ├── parse.rs          # line-oriented parser for the input format
│   └── analysis.rs       # typestate lattice, fixpoint dataflow, and the join
└── tests_mir/
    ├── can_sleep.txt     # C-side CanSleep summary (one symbol per line)
    ├── t1..t7 *.mir      # kernel-shaped test inputs
    └── expected/         # frozen expected outputs for diffing
```

---

## Reproducibility

- **No dependencies** beyond the Rust standard library, so `cargo build` needs no
  network access and the build is deterministic.
- **Deterministic output**: all reporting is sorted by `(function, block)`.
- **Frozen expectations**: `tests_mir/expected/` holds the golden outputs;
  `run_tests.sh` fails on any diff.
- Built and tested with `rustc` 1.75. No nightly features are used.

---

## Roadmap

In priority order — each item moves the artifact closer to the O1/O3 milestones:

1. **Ingest real MIR via `dylint`.** Replace the text IR front-end with a
   `dylint` lint that reads genuine post-drop-elaboration MIR from a real
   Rust-for-Linux driver, keeping this same lattice and join. This is the O1→P2
   step and the highest-value next move.
2. **Consume the real C-side summary.** Emit `can_sleep.txt` directly from the
   LLVM *Sleepability* pass on a linked kernel slice and run a genuine cross-IR
   end-to-end check.
3. **More controls.** A nested two-guard case (drop one, stay atomic) and an
   in-band `in_atomic()` guard (deterministic false-positive suppression, as the
   evaluation plan promises).
4. **Beyond v1.** Indirect / function-pointer calls and the C→Rust callback path,
   following the deferred plan in the proposal rather than improvising.

---

## Relationship to the C-side artifact

The C-side *Sleepability* pass computes `CanSleep(f)` over the C call graph at
LLVM IR — a function may sleep if it, or anything transitively reachable, calls a
blocking primitive or allocates with a sleeping GFP flag. It emits that set as a
summary. `guardstate` computes the Rust-side context and consumes that summary at
the join. The two artifacts share a contract — the summary file — and together
form a miniature end-to-end CLSC across both IRs. Neither claims to be the whole
system; each is one honest brick.

---

## References

- J.-J. Bai, J. Lawall, S.-M. Hu. *Effective detection of sleep-in-atomic-context
  bugs in the Linux kernel.* ACM TOCS 36(4), 2020. (DSAC — the C-side lineage.)
- J. Corbet. *Preventing atomic-context violations in Rust code with klint.*
  LWN.net, 2023. (The Rust-side context tracking CLSC automates the FFI half of.)
- K. Suzuki, K. Ishiguro, K. Kono. *Balancing analysis time and bug detection:
  daily development-friendly bug detection in Linux.* USENIX ATC, 2024. (FiTx —
  the development-time budget CLSC targets.)
- R. E. Strom, S. Yemini. *Typestate: a programming language concept for
  enhancing software reliability.* IEEE TSE, 1986. (The typestate foundation.)
- C. Lattner, V. Adve. *LLVM: a compilation framework for lifelong program
  analysis & transformation.* CGO, 2004.

See the research proposal for the full bibliography and the CLSC design.

---

## Authorship, license, and citation

**Author:** Annanya Sood — <annanyas0142@gmail.com>

I scoped this project and own its design. The decisions are mine: to implement
the Rust half and the join, so that the two artifacts share a contract — the
summary file — and together demonstrate the split invariant end to end; to
represent the abstract state as the *set* of live preemption-disabling guards
rather than a single bit, so nested guards drop precisely; to take the union at
control-flow merges, which is the sound direction for a detector; to key guard
liveness on drop and `StorageDead` points rather than lexical scope; to treat
`mem::forget` conservatively, since the guard's `Drop` never runs and preemption
stays disabled; to make conditional acquisition path-sensitive so `try_lock`
fires only where it succeeded; and to prove the lattice on MIR-shaped input,
symmetric to how the C-side pass is proven on kernel-shaped `.c` files, rather
than claiming a `rustc` frontend the artifact does not have. Each of the seven
tests isolates one behaviour of the lattice, with a near-miss control for
precision.

The implementation was written with AI assistance (Claude, by Anthropic) working
to that direction. I can account for each component and the reasoning behind it,
and I take responsibility for the artifact as published.

**License:** GPL-2.0 (see [`LICENSE`](LICENSE)). GPL-2.0 is chosen to match the
Linux kernel, since this work targets Rust-for-Linux. Each source file carries an
`SPDX-License-Identifier: GPL-2.0` header per kernel convention.

**Design document:** see [`DESIGN.md`](DESIGN.md) for the architecture, the
lattice and transfer functions, the design decisions and their rationale, the
soundness argument, the testing strategy, and the roadmap.

**Citing this work:** if you reference `guardstate`, please cite it as the
Rust-side component (objective O1 and the call-site join) of the CLSC project by
Annanya Sood, and see the research proposal for the full design and evaluation
plan.
