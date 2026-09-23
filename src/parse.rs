// SPDX-License-Identifier: GPL-2.0
// Author: Annanya Sood <annanyas0142@gmail.com>

//! A tiny, line-oriented parser for the MIR-shaped input format.
//!
//! Grammar (one token stream per line, `#` starts a comment):
//!
//!   fn NAME                       -- begins a function; first bb is the entry
//!   bbN:                          -- begins a basic block
//!     LOCAL = acquire KIND        -- unconditional guard acquire (statement)
//!     drop LOCAL                  -- guard drop / StorageDead (statement)
//!     storage_dead LOCAL          -- same as drop
//!     call SYMBOL -> bbN          -- FFI/extern call terminator
//!     drop_call LOCAL -> bbN      -- core::mem::drop(LOCAL) terminator
//!     forget_call LOCAL -> bbN    -- core::mem::forget(LOCAL) terminator
//!     try LOCAL = KIND -> bbSucc bbFail   -- conditional acquire (spin_trylock)
//!     goto bbN                    -- unconditional branch
//!     ret                         -- return
//!
//! KIND is a kernel acquire name (spin_lock, raw_spin_lock, mutex_lock, ...).

use crate::ir::*;
use std::collections::BTreeMap;

pub fn parse_module(src: &str) -> Result<Vec<Function>, String> {
    let mut funcs = Vec::new();
    let mut cur: Option<Function> = None;
    let mut cur_bb: Option<Block> = None;

    // Helper to flush the in-progress block into the current function.
    fn flush_bb(f: &mut Function, bb: &mut Option<Block>) {
        if let Some(b) = bb.take() {
            f.order.push(b.id.clone());
            f.blocks.insert(b.id.clone(), b);
        }
    }
    fn flush_fn(funcs: &mut Vec<Function>, cur: &mut Option<Function>, bb: &mut Option<Block>) {
        if let Some(mut f) = cur.take() {
            flush_bb(&mut f, bb);
            funcs.push(f);
        }
    }

    for (lineno, raw) in src.lines().enumerate() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let toks: Vec<&str> = line.split_whitespace().collect();
        let err = |m: &str| format!("line {}: {} (`{}`)", lineno + 1, m, line);

        // fn NAME
        if toks[0] == "fn" {
            flush_fn(&mut funcs, &mut cur, &mut cur_bb);
            let name = toks.get(1).ok_or_else(|| err("missing function name"))?;
            cur = Some(Function {
                name: name.to_string(),
                entry: String::new(),
                blocks: BTreeMap::new(),
                order: Vec::new(),
            });
            continue;
        }

        let f = cur.as_mut().ok_or_else(|| err("statement outside a function"))?;

        // bbN:
        if line.ends_with(':') {
            flush_bb(f, &mut cur_bb);
            let id = line.trim_end_matches(':').to_string();
            if f.entry.is_empty() {
                f.entry = id.clone();
            }
            cur_bb = Some(Block { id, stmts: Vec::new(), term: Term::Ret });
            continue;
        }

        let b = cur_bb.as_mut().ok_or_else(|| err("statement outside a basic block"))?;

        // LOCAL = acquire KIND   |   try LOCAL = KIND -> succ fail
        if toks.len() >= 3 && toks[1] == "=" && toks[2] == "acquire" {
            let kind = GuardKind::from_acquire_name(toks.get(3).ok_or_else(|| err("acquire needs a kind"))?);
            b.stmts.push(Stmt::Acquire { local: toks[0].to_string(), kind });
            continue;
        }

        match toks[0] {
            "drop" => b.stmts.push(Stmt::Drop { local: toks[1].to_string() }),
            "storage_dead" => b.stmts.push(Stmt::StorageDead { local: toks[1].to_string() }),
            "goto" => b.term = Term::Goto { target: toks[1].to_string() },
            "ret" => b.term = Term::Ret,
            "call" => {
                // call SYMBOL -> bbN
                let arrow = toks.iter().position(|t| *t == "->").ok_or_else(|| err("call needs ->"))?;
                b.term = Term::Call {
                    symbol: toks[1..arrow].join(" "),
                    target: toks[arrow + 1].to_string(),
                };
            }
            "drop_call" => {
                let arrow = toks.iter().position(|t| *t == "->").ok_or_else(|| err("drop_call needs ->"))?;
                b.term = Term::MemDrop { local: toks[1].to_string(), target: toks[arrow + 1].to_string() };
            }
            "forget_call" => {
                let arrow = toks.iter().position(|t| *t == "->").ok_or_else(|| err("forget_call needs ->"))?;
                b.term = Term::MemForget { local: toks[1].to_string(), target: toks[arrow + 1].to_string() };
            }
            "try" => {
                // try LOCAL = KIND -> succ fail
                if toks.get(2) != Some(&"=") {
                    return Err(err("try syntax: try LOCAL = KIND -> succ fail"));
                }
                let arrow = toks.iter().position(|t| *t == "->").ok_or_else(|| err("try needs ->"))?;
                let kind = GuardKind::from_acquire_name(toks[3]);
                b.term = Term::TryAcquire {
                    local: toks[1].to_string(),
                    kind,
                    success: toks[arrow + 1].to_string(),
                    fail: toks.get(arrow + 2).ok_or_else(|| err("try needs fail target"))?.to_string(),
                };
            }
            _ => return Err(err("unrecognised statement")),
        }
    }

    flush_fn(&mut funcs, &mut cur, &mut cur_bb);
    Ok(funcs)
}
