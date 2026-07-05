// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::{path::Path, rc::Rc};

use stack::{FunctionReport, ResolvedNode, Resolver};

fn main() {
    let file = "../../target/gimlet-c/dist/host_sp_comms.tmp";
    let items =
        stack::extract_function_items(Path::new(file), "host_sp_comms", false)
            .unwrap();

    let FunctionReport {
        function_items,
        addr_to_frame_size,
        names,
    } = items;

    let mut resolver = Resolver::new(function_items);
    let node = resolver.resolve_by_name("_start").unwrap();
    node.debug_all();

    let mut missing = vec![];
    for (addr, size) in addr_to_frame_size.iter() {
        let name = names.get(addr).unwrap();
        if !resolver.all_resolved.contains_key(addr) {
            println!("WARN: missing {name} => {size}");
            missing.push((*addr, name));
        }
    }

    let mut found = vec![];
    for (addr, name) in missing {
        println!("probing {name}...");
        let node = resolver.resolve_addr(addr).unwrap();
        println!("  {:?} + {}", node.local_size, node.max_children);
        node.debug_all();
        println!("---");
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
        n.debug_all();
        println!("---");
        sum += n.max_stack();
    }

    println!();
    println!("Worst chain:");
    let chain = node.worst_chain();
    for n in chain {
        println!("{} - {}", n.name, n.max_stack());
    }

    println!(
        "max stack: {} + fudge ({}) = {}",
        node.max_stack(),
        sum,
        node.max_stack() + sum
    );
}
