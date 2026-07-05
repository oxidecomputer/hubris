// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::{collections::BTreeSet, path::Path};

use stack::FunctionReport;

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

    for (addr, name) in names.iter() {
        println!("{addr:08X} - {name}");
    }

    println!();

    for (addr, size) in addr_to_frame_size.iter() {
        println!("{addr:08X} - {size}");
    }

    println!();

    for (addr, item) in function_items.iter() {
        print!("{addr:08X} ");

        if let Some(n) = names.get(&addr) {
            print!("{n} ");
        } else {
            print!("anon@{addr:08X} ");
        }
        if let Some(n) = addr_to_frame_size.get(&addr) {
            println!("- {n} bytes");
        } else {
            println!("- ??? bytes");
        }

        for subitem in item.calls.iter() {
            print!("  => ");
            if let Some(n) = names.get(&subitem) {
                print!("{n} ");
            } else {
                print!("anon@{subitem:08X} ");
            }
            if let Some(n) = addr_to_frame_size.get(&subitem) {
                println!("- {n} bytes");
            } else {
                println!("- ??? bytes");
            }
        }
    }

    let as_fi = function_items.keys().copied().collect::<BTreeSet<_>>();
    let as_a2f = addr_to_frame_size.keys().copied().collect::<BTreeSet<_>>();
    let as_names = names.keys().copied().collect::<BTreeSet<_>>();

    let fi_a2f = as_fi.difference(&as_a2f);
    let a2f_names = as_a2f.difference(&as_names);
    let names_fi = as_names.difference(&as_fi);

    println!("fi_a2f:");
    for addr in fi_a2f {
        println!("- {addr:08X}");
    }
    println!();

    println!("a2f_names:");
    for addr in a2f_names {
        println!("- {addr:08X}");
    }
    println!();

    println!("names_fi:");
    for addr in names_fi {
        println!("- {addr:08X}");
    }
    println!();
}
