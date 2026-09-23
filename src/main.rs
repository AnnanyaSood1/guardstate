// SPDX-License-Identifier: GPL-2.0
// Author: Annanya Sood <annanyas0142@gmail.com>

//! guardstate — Rust-side atomic-context typestate analyzer for CLSC (O1),
//! with a call-site join against a C-side sleepability summary (O3 teaser).
//!
//! Usage:
//!   guardstate <input.mir> [--summary <can_sleep.txt>]
//!
//! Input is MIR-shaped text (see src/parse.rs). The optional summary file lists
//! CanSleep callee symbols, one per line (`#` comments allowed) — exactly the
//! artifact the C-side LLVM Sleepability pass emits. With a summary, guardstate
//! prints violations; without one, it prints only the per-site context verdicts.

mod analysis;
mod ir;
mod parse;

use std::collections::BTreeSet;
use std::process::exit;

fn load_summary(path: &str) -> BTreeSet<String> {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|e| { eprintln!("error: cannot read summary {}: {}", path, e); exit(2); });
    text.lines()
        .map(|l| l.split('#').next().unwrap_or("").trim())
        .filter(|l| !l.is_empty())
        .map(|l| l.to_string())
        .collect()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: {} <input.mir> [--summary <can_sleep.txt>]", args[0]);
        exit(2);
    }
    let input = &args[1];
    let mut summary: Option<BTreeSet<String>> = None;
    let mut i = 2;
    while i < args.len() {
        if args[i] == "--summary" {
            summary = Some(load_summary(args.get(i + 1).unwrap_or_else(|| {
                eprintln!("error: --summary needs a path"); exit(2);
            })));
            i += 2;
        } else {
            eprintln!("error: unknown argument {}", args[i]);
            exit(2);
        }
    }

    let src = std::fs::read_to_string(input)
        .unwrap_or_else(|e| { eprintln!("error: cannot read {}: {}", input, e); exit(2); });
    let funcs = parse::parse_module(&src)
        .unwrap_or_else(|e| { eprintln!("parse error: {}", e); exit(2); });

    let mut sites = Vec::new();
    for f in &funcs {
        sites.extend(analysis::analyze_fn(f));
    }
    sites.sort_by(|a, b| (a.func.as_str(), a.bb.as_str()).cmp(&(b.func.as_str(), b.bb.as_str())));

    println!("== atomic-context at FFI call sites ==");
    for s in &sites {
        println!(
            "{}::{} -> {:<28} [{}]",
            s.func,
            s.bb,
            s.symbol,
            if s.atomic_held { "ATOMIC-HELD" } else { "SLEEPABLE" }
        );
    }

    if let Some(cs) = summary {
        let violations = analysis::join(&sites, &cs);
        println!("\n== sleep-in-atomic violations (State=AtomicHeld & callee=CanSleep) ==");
        if violations.is_empty() {
            println!("(none)");
        } else {
            for v in &violations {
                println!("VIOLATION {}::{} calls {} while holding atomic context", v.func, v.bb, v.symbol);
            }
        }
        println!("\nsummary: {} call site(s), {} violation(s)", sites.len(), violations.len());
    }
}
