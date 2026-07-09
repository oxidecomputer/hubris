// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::{path::Path, rc::Rc};

use build_stack::{FunctionReport, ResolvedNode, Resolver};

fn main() {
    let file = "target/oxide-rot-1-selfsigned/dist/attest.tmp";
    // build_stack::chunkify_text(Path::new(file));
    let items =
        build_stack::extract_function_items(Path::new(file), false).unwrap();

    let FunctionReport {
        function_items,
        addr_to_frame_size: _,
    } = items;

    for (addr, item) in function_items.iter() {
        println!("{addr:08X}, {item:08X?}");
    }

    let mut resolver = Resolver::new(function_items);
    let node = resolver.resolve_by_name("_start").unwrap();
    // node.debug_all();

    println!();
    println!();
    let mut missing = vec![];
    for (addr, fd) in resolver.fn_items.clone() {
        if !resolver.all_resolved.contains_key(&addr) {
            println!("WARN: missing {} => {:?}", fd.name, fd.frame_size);
            missing.push((addr, fd));
        }
    }

    println!();
    let mut found = vec![];
    for (addr, fd) in missing {
        println!("probing {}...", fd.name);
        let node = resolver.resolve_addr(addr).unwrap();
        found.push(node);
    }

    println!("Shaking down...");
    let mut last_len = found.len();
    loop {
        let mut to_keep = vec![];
        while let Some(val) = found.pop() {
            if found
                .iter()
                .any(|f: &Rc<ResolvedNode>| f.is_same_or_child_of(&val))
                || to_keep
                    .iter()
                    .any(|f: &Rc<ResolvedNode>| f.is_same_or_child_of(&val))
            {
                continue;
            } else {
                to_keep.push(val);
            }
        }
        found = to_keep;
        if found.len() == last_len {
            break;
        } else {
            last_len = found.len();
        }
    }

    println!("--- shook ---");
    let mut sum = 0;
    for n in found.iter() {
        println!("{} - {:?} + {}", n.name, n.local_size, n.max_children);
        sum += n.max_stack();
    }

    println!();
    println!("------------");
    println!("Worst chain:");
    println!("------------");
    let chain = node.worst_chain();
    let mut worst_sum = 0;
    for n in chain {
        worst_sum += n.local_size.unwrap_or(0);
        println!("{} - [+{:?} => {}]", n.name, n.local_size, worst_sum);
    }

    println!();
    println!(
        "max stack: {} + fudge ({}) = {}",
        node.max_stack(),
        sum,
        node.max_stack() + sum
    );

    println!();
    println!("{} functions resolved.", resolver.all_resolved.len());
}
