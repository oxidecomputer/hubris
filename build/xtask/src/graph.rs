// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::collections::{BTreeMap, HashSet};
use std::fs::File;
use std::hash::Hash;
use std::io::Write;
use std::path::Path;

use anyhow::{anyhow, Result};

use crate::config::Config;

/// Generate a directed graph of task priorities and task_slot
/// dependencies.
pub fn task_graph(app_toml: &Path, path: &Path) -> Result<()> {
    // Generate dot syntax for a graph of process priorities.
    // Collect each task in a priority group
    // Collect each edge
    let mut priorities = BTreeMap::new();
    let mut edges = Vec::new();
    let mut dot = File::create(path)?;
    let mut ranks = HashSet::new();
    let toml = Config::from_file(app_toml)?;

    #[derive(Debug)]
    struct Edge<'a> {
        from: &'a String,
        to: &'a String,
        inverted: bool,
    }

    #[derive(Debug, Eq, PartialEq, PartialOrd, Ord, Hash)]
    struct Rank {
        from: u8,
        to: u8,
    }

    for (name, task) in toml.tasks.iter() {
        priorities.entry(task.priority).or_insert_with(Vec::new);
        if let Some(v) = priorities.get_mut(&task.priority) {
            v.push(name.to_string());
        }
        for callee in task.task_slots.values() {
            let p = toml
                .tasks
                .get(callee)
                .ok_or_else(|| anyhow!("Invalid task-slot: {}", callee))?
                .priority;
            let inverted = p >= task.priority && name != callee;
            edges.push(Edge {
                from: name,
                to: callee,
                inverted,
            });
            let rank = Rank {
                from: task.priority,
                to: p,
            };
            if !ranks.contains::<Rank>(&rank) {
                ranks.insert(rank);
            }
        }
    }

    writeln!(dot, "digraph tasks {{")?;
    writeln!(
        dot,
        "  labelloc=\"t\";\n  label=\"{}\";",
        toml.app_toml_path.display()
    )?;
    for key in priorities.keys() {
        if let Some(v) = priorities.get(key) {
            writeln!(dot, "  {{\n    edge [ style=invis ];\n    rank=same;")?;
            for name in v {
                writeln!(
                    dot,
                    "    {name} [ label=\"{name}\\n{key}\", shape=box ];",
                )?;
            }
            writeln!(dot, "  }}")?;
        }
    }
    for edge in edges {
        let attr = if edge.inverted {
            r#" [color=red, style=dashed, penwidth=3, constraint=false, label="BAD"]"#
        } else {
            " [color=green]"
        };
        writeln!(dot, "  {} -> {}{};", edge.from, edge.to, attr)?;
    }
    let keys: Vec<&u8> = priorities.keys().collect();
    let mut first = false;
    for low_high in keys.windows(2) {
        let low = low_high[0];
        let high = low_high[1];
        if !ranks.contains::<Rank>(&Rank {
            from: *high,
            to: *low,
        }) {
            if !first {
                first = true;
                writeln!(
                    dot,
                    "\n  # Force row ranking by priorities {keys:?}",
                )?;
            }
            writeln!(dot, "  # Adding {high} -> {low}")?;
            let high_name = &priorities.get(high).unwrap()[0];
            let low_name = &priorities.get(low).unwrap()[0];
            writeln!(dot, "  {high_name} -> {low_name} [style=invis];")?;
        }
    }
    writeln!(dot, "}}")?;

    Ok(())
}
