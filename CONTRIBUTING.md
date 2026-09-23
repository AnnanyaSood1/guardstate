<!--
SPDX-License-Identifier: GPL-2.0
Author: Annanya Sood <annanyas0142@gmail.com>
-->

# Contributing to guardstate

Thanks for your interest. `guardstate` is a research prototype — the Rust-side
atomic-context typestate analyzer of the CLSC project (see
[`README.md`](README.md) and [`DESIGN.md`](DESIGN.md)). This guide covers
building from scratch, running and extending the tests, the coding conventions,
and the planned directions where help is most useful.

## Building from scratch (Ubuntu)

1. Install a Rust toolchain with `rustup` (preferred over the distro package, so
   you also get `rustfmt` and `clippy`, and can switch toolchains later):

   ```sh
   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
   source "$HOME/.cargo/env"
   rustc --version   # 1.75 or newer
   ```

2. Build and test:

   ```sh
   cargo build
   ./run_tests.sh    # builds, runs every test, diffs against expected
   ```

3. Run the analyzer on a single input:

   ```sh
   ./target/debug/guardstate tests_mir/t1_spinlock_sleep.mir --summary tests_mir/can_sleep.txt
   ```

No external crates are required; the build is offline and deterministic.

## Repository layout

See the [Project layout](README.md#project-layout) section of the README. In
short: `src/ir.rs` (IR types), `src/parse.rs` (parser), `src/analysis.rs`
(lattice, fixpoint, join), `src/main.rs` (CLI); `tests_mir/` holds inputs, the
shared C-side summary, and frozen expected outputs.

## Adding a test

Tests are golden-file based, one behaviour per case:

1. Write a new `tests_mir/tN_shortname.mir` using the IR grammar documented in
   the README (and in `src/parse.rs`).
2. Generate its expected output once, then eyeball it for correctness:

   ```sh
   cargo build
   ./target/debug/guardstate tests_mir/tN_shortname.mir --summary tests_mir/can_sleep.txt \
     > tests_mir/expected/tN_shortname.txt
   ```

3. Confirm the whole suite still passes: `./run_tests.sh`.

Prefer adding **paired** cases — a positive control that must fire and a
near-miss negative control that must not — since that is what demonstrates
precision rather than mere sensitivity.

## Coding conventions

- **SPDX + author header** on every source file:
  `// SPDX-License-Identifier: GPL-2.0` then an author line.
- **Formatting:** run `cargo fmt` before committing; keep `cargo build` warning-clean.
- **Determinism:** all reported output must be sorted by `(function, block)`; no
  iteration order may leak into results (that is what makes the golden diffs
  stable).
- **Clarity over cleverness:** this is a research artifact meant to be read and
  audited; favour obvious correctness.

## Commit and sign-off (DCO)

This project follows the kernel's Developer Certificate of Origin. Sign your
commits so authorship and provenance are explicit:

```sh
git commit -s -m "component: short imperative summary"
```

The `-s` adds a `Signed-off-by:` line. Write commit subjects in the imperative
mood, keep them short, and explain the *why* in the body.

## Potential expansions (where help is most useful)

These follow the CLSC objectives and the phased plan in `DESIGN.md` §13,
roughly in priority order:

1. **Real MIR via `dylint` (O1 → P2).** Replace the text front-end with a
   `dylint` lint that reads post-drop-elaboration MIR from a real
   Rust-for-Linux driver, keeping the current lattice and join unchanged. This
   is the single highest-value expansion: it turns "correct on MIR-shaped
   inputs" into "runs on real kernel Rust." Needs a pinned nightly and
   `rustc_private`; the front-end is deliberately isolated to make this a local
   change.
2. **Real C-side summary integration (O2).** Consume a `can_sleep.txt` emitted
   directly by the LLVM Sleepability pass on a linked kernel slice, for a
   genuine cross-IR, end-to-end run rather than a hand-written summary.
3. **Precision controls.** Add a nested two-guard case (drop one, stay atomic);
   recognise in-band guards such as `in_atomic()` checks; add deterministic,
   traceable path pruning — each justified by a measurable false-positive drop
   on fault-injection controls, never by opaque filtering.
4. **`PREEMPT_RT` awareness.** Make the guard-kind classification configurable,
   since under `PREEMPT_RT` `spinlock_t` becomes a sleeping lock and only raw
   spinlocks and explicit preempt/IRQ-disabling guards raise context.
5. **Beyond v1 (research frontier).** Indirect / function-pointer and vtable
   dispatch (connection-based alias analysis on the C side; callsite-sensitive
   MIR pointer analysis on the Rust side), and the mirror **C→Rust callback**
   invariant, where atomic context originates in C and the callback sleeps.
6. **Developer ergonomics.** A `--json` output mode for tooling/CI integration;
   richer diagnostics that print the guard-acquisition site alongside the
   offending call site.

If you plan to take on one of these, opening an issue first to align on scope is
appreciated.

## Contact

Annanya Sood &lt;annanyas0142@gmail.com&gt;
