// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Implements patching of TOML documents
//!
//! The `toml_edit` crate is great, but it has a major limitation: tables are
//! ordered with a global `position`.  This means that to insert a new table,
//! you have to shift everthing after that table downwards, and adjust relative
//! positions within the new table.
//!
//! This crate exposes a single function [`merge_toml_documents`] which does
//! this for you!

use anyhow::{anyhow, bail, Result};
use std::collections::BTreeMap;
use toml_edit::{visit::Visit, visit_mut::VisitMut};

pub fn merge_toml_documents(
    original: &mut toml_edit::DocumentMut,
    mut patches: toml_edit::DocumentMut,
) -> Result<()> {
    // Find offsets where we need to insert gaps for incoming patches
    let mut offsets = BTreeMap::new();
    compute_offsets(original, &patches, &mut offsets)?;

    // Convert from single to cumulative offsets.  Since this is in a BTreeMap,
    // it's already sorted, so we accumulate in a single pass.
    let mut sum = 0;
    for i in offsets.values_mut() {
        let prev = *i;
        *i += sum;
        sum += prev;
    }

    // Apply offsets, adding gaps to the original document
    let mut visitor = OffsetVisitor { offsets: &offsets };
    visitor.visit_document_mut(original);

    // Now that we've opened up gaps, we can splice in the new data
    merge_toml_tables(original.as_table_mut(), &mut patches)
}

/// Computes offsets that will be applied when `patches` is merged
///
/// Values are accumulated into `offsets`
fn compute_offsets(
    original: &toml_edit::Table,
    patches: &toml_edit::Table,
    offsets: &mut BTreeMap<isize, usize>,
) -> Result<()> {
    for (k, v) in patches.iter() {
        if let Some(u) = original.get(k) {
            if u.type_name() != v.type_name() {
                bail!(
                    "type mismatch for '{k}': {} != {}",
                    u.type_name(),
                    v.type_name()
                );
            }
            use toml_edit::Item;
            match u {
                Item::None | Item::Value(..) => (),
                Item::Table(u) => {
                    // Recurse!
                    compute_offsets(u, v.as_table().unwrap(), offsets)?;
                }
                Item::ArrayOfTables(..) => {
                    let mut visitor = TableRangeVisitor::default();
                    visitor.visit_item(u);
                    let range_in = visitor.range.ok_or_else(|| {
                        anyhow!("cannot append to an empty array of tables")
                    })?;

                    // Find the range spanned by the incoming tables, which will
                    // be appended to the list `u`
                    let mut visitor = TableRangeVisitor::default();
                    visitor.visit_item(v);
                    let patch_span = visitor.range.ok_or_else(|| {
                        anyhow!("cannot append empty array of tables")
                    })?;

                    // Record the shift here
                    offsets.insert(range_in.end, patch_span.len());
                }
            }
        } else {
            // We are going to insert our new values after all of the old values
            //
            // First, we need to find the maximum position in our original table
            let mut visitor = TableRangeVisitor::default();
            visitor.visit_table(original);
            let last = visitor.range.unwrap().end;

            // We'll be applying an offset based on the size of the incoming
            // list of patches (in table positions)
            let mut visitor = TableRangeVisitor::default();
            visitor.visit_table(patches);
            if let Some(r) = visitor.range {
                offsets.insert(last + 1, r.len());
            }
        }
    }
    Ok(())
}

/// Accumulates the full range of table positions
#[derive(Default)]
struct TableRangeVisitor {
    range: Option<std::ops::Range<isize>>,
}

impl<'doc> Visit<'doc> for TableRangeVisitor {
    fn visit_table(&mut self, t: &'doc toml_edit::Table) {
        if let Some(pos) = t.position() {
            self.range = Some(match self.range.take() {
                Some(r) => r.start.min(pos)..r.end.max(pos + 1),
                None => pos..pos + 1,
            });
        }
        // call the default implementation to recurse
        self.visit_table_like(t);
    }
}

/// Applies an offset to every table position
#[derive(Default)]
struct TableShiftVisitor {
    offset: isize,
}

impl VisitMut for TableShiftVisitor {
    fn visit_table_mut(&mut self, t: &mut toml_edit::Table) {
        if let Some(pos) = t.position() {
            let pos: isize = pos.try_into().unwrap();
            t.set_position((pos + self.offset).try_into().unwrap())
        }
        // call the default implementation to recurse
        self.visit_table_like_mut(t);
    }
}

/// Applies an offset that varies based on table position
struct OffsetVisitor<'a> {
    /// Map from position in original table to offset
    ///
    /// This is a sparse map containing **cumulative** offsets.
    ///
    /// The offset at position `i` is `self.offsets[j]`, where `j` is the
    /// largest key such that `j <= i`.
    offsets: &'a BTreeMap<isize, usize>,
}

impl<'a> VisitMut for OffsetVisitor<'a> {
    fn visit_table_mut(&mut self, t: &mut toml_edit::Table) {
        if let Some(pos) = t.position() {
            // Find the largest offset with a value <= pos, which determines
            // the cumulative offset at this point in the document.
            //
            // If `pos` is _before_ the first offset in the table, then return a
            // base case with no offset, i.e. (0, 0)
            let (prev_pos, offset) =
                self.offsets.range(0..=pos).next_back().unwrap_or((&0, &0));
            assert!(*prev_pos <= pos); // sanity-checking
            t.set_position(isize::try_from(*offset).unwrap() + pos);
        }
        self.visit_table_like_mut(t);
    }
}

/// Merges a pair of TOML tables
///
/// The incoming `patches` table is modified during execution to put its
/// position at the end of the original table.
///
/// When this function is called, `original` must include gaps for `patches`
fn merge_toml_tables(
    original: &mut toml_edit::Table,
    patches: &mut toml_edit::Table,
) -> Result<()> {
    for (k, v) in patches.iter_mut() {
        if let Some(u) = original.get_mut(k.get()) {
            assert_eq!(u.type_name(), v.type_name()); // already checked
            use toml_edit::Item;
            match u {
                Item::None => bail!("can't patch `None`"),
                Item::Value(u) => {
                    // I'm not sure whether it's possible for the Item
                    // type_name to match and Value type_name to *not*
                    // match, but better safe than sorry here.
                    let v = v.as_value().unwrap();
                    if u.type_name() != v.type_name() {
                        bail!(
                            "type mismatch for '{}': {} != {}",
                            k.to_string(),
                            u.type_name(),
                            v.type_name()
                        );
                    }

                    use toml_edit::Value;
                    match u {
                        // Single values replace the previous value
                        Value::Float(..)
                        | Value::String(..)
                        | Value::Integer(..)
                        | Value::Boolean(..)
                        | Value::Datetime(..) => *u = v.clone(),

                        // Inline tables are not yet supported, but should be
                        // merged once we get around to implementing it
                        Value::InlineTable(..) => {
                            bail!(
                                "patching inline tables is not yet implemented"
                            );
                        }
                        // Arrays are extended
                        Value::Array(u) => {
                            u.extend(v.as_array().unwrap().iter().cloned());
                        }
                    }
                }
                Item::Table(u) => {
                    // Recurse!
                    merge_toml_tables(u, v.as_table_mut().unwrap())?;
                }
                Item::ArrayOfTables(arr) => {
                    // Compute an offset based on table position
                    let mut visitor = TableRangeVisitor::default();
                    visitor.visit_array_of_tables(arr);
                    let range_in = visitor.range.unwrap();
                    let last = range_in.end;

                    let mut visitor = TableRangeVisitor::default();
                    visitor.visit_item(v);
                    let start =
                        visitor.range.map(|r| r.start as isize).unwrap();
                    let offset = last as isize - start;

                    // Apply that offset to the incoming tables
                    let mut visitor = TableShiftVisitor { offset };
                    visitor.visit_item_mut(v);

                    // Merge by extending the table array
                    arr.extend(v.as_array_of_tables().unwrap().iter().cloned());
                }
            }
        } else {
            let mut visitor = TableRangeVisitor::default();
            visitor.visit_table(original);
            let last = visitor.range.unwrap().end;

            let mut visitor = TableRangeVisitor::default();
            visitor.visit_item(v);
            let start = visitor.range.map(|r| r.start).unwrap_or(0);
            let offset = last - start;

            // Apply that offset to the incoming tables
            let mut visitor = TableShiftVisitor { offset };
            visitor.visit_item_mut(v);

            // Merge by inserting the new element
            original.insert(k.get(), v.clone());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use indoc::indoc;

    fn patch_and_compare(a: &str, b: &str, out: &str) {
        let mut a: toml_edit::DocumentMut = a.parse().unwrap();
        let b = b.parse().unwrap();
        merge_toml_documents(&mut a, b).unwrap();
        if a.to_string() != out {
            eprintln!("patching failed.  Got result:");
            eprintln!("{a}");
            eprintln!("----------------");
            eprintln!("{out}");
        }
        assert_eq!(a.to_string(), out);
    }
    #[test]
    fn test_patching() {
        patch_and_compare(
            indoc! {r#"
                name = "foo"
                age = 37
            "#},
            indoc! {r#"
                age = 38
            "#},
            indoc! {r#"
                name = "foo"
                age = 38
            "#},
        );
        patch_and_compare(
            indoc! {r#"
                name = "foo"
                age = 37

                [nested]
                hi = "there"
            "#},
            indoc! {r#"
                age = 38

                [nested]
                omg = "bbq"
            "#},
            indoc! {r#"
                name = "foo"
                age = 38

                [nested]
                hi = "there"
                omg = "bbq"
            "#},
        );
        patch_and_compare(
            indoc! {r#"
                name = "foo"
                age = 37

                [config]
                [[config.i2c.buses]]
                i2c0 = "fine"

                [config.spi]
                spi1 = "great"
            "#},
            indoc! {r#"
                [[config.i2c.buses]]
                i2c4 = { status = "running" }
                [config.pcie]
                presence = false
            "#},
            indoc! {r#"
                name = "foo"
                age = 37

                [config]
                [[config.i2c.buses]]
                i2c0 = "fine"
                [[config.i2c.buses]]
                i2c4 = { status = "running" }

                [config.spi]
                spi1 = "great"
                [config.pcie]
                presence = false
            "#},
        );
        // Same as above, but swap the order in the patch
        patch_and_compare(
            indoc! {r#"
                name = "foo"
                age = 37

                [config]
                [[config.i2c.buses]]
                i2c0 = "fine"

                [config.spi]
                spi1 = "great"
            "#},
            indoc! {r#"
                bar = "foo"
                [config.pcie]
                presence = false
                [[config.i2c.buses]]
                i2c4 = { status = "running" }
            "#},
            indoc! {r#"
                name = "foo"
                age = 37
                bar = "foo"

                [config]
                [[config.i2c.buses]]
                i2c0 = "fine"
                [[config.i2c.buses]]
                i2c4 = { status = "running" }

                [config.spi]
                spi1 = "great"
                [config.pcie]
                presence = false
            "#},
        );
        patch_and_compare(
            indoc! {r#"
                name = "foo"
                age = 37

                [block]
                great = true

                [config]
                [[config.i2c.buses]]
                i2c0 = "fine"

                [config.spi]
                spi1 = "great"
            "#},
            indoc! {r#"
                bar = "foo"
                [config.pcie]
                presence = false
                [[config.i2c.buses]]
                i2c4 = { status = "running" }
            "#},
            indoc! {r#"
                name = "foo"
                age = 37
                bar = "foo"

                [block]
                great = true

                [config]
                [[config.i2c.buses]]
                i2c0 = "fine"
                [[config.i2c.buses]]
                i2c4 = { status = "running" }

                [config.spi]
                spi1 = "great"
                [config.pcie]
                presence = false
            "#},
        );
        patch_and_compare(
            indoc! {r#"
                name = "foo"

                [tasks.jefe]
                features = ["hello", "world"]
                great = true

                [config]
                [[config.i2c.buses]]
                i2c0 = "fine"

                [config.spi]
                spi1 = "great"
            "#},
            indoc! {r#"
                tasks.jefe.features = ["aaaaahhhh!"]
                [config.pcie]
                presence = false
                [[config.i2c.buses]]
                i2c4 = { status = "running" }
            "#},
            indoc! {r#"
                name = "foo"

                [tasks.jefe]
                features = ["hello", "world","aaaaahhhh!"]
                great = true

                [config]
                [[config.i2c.buses]]
                i2c0 = "fine"
                [[config.i2c.buses]]
                i2c4 = { status = "running" }

                [config.spi]
                spi1 = "great"
                [config.pcie]
                presence = false
            "#},
        );
    }

    #[test]
    fn heterogenous_tables() {
        let cfg = indoc! {r#"
            [tasks.jefe]
            features = ["hello", "world"]
            config.do-not-dump = ["chaos"]

            [tasks.jefe.config.allowed-callers]
            set_reset_reason = ["sys"]
        "#};
        patch_and_compare(cfg, "", cfg);
    }

    #[test]
    fn merge_heterogenous_tables() {
        patch_and_compare(
            indoc! {r#"
                [tasks.jefe]
                features = ["hello", "world"]

                [tasks.jefe.config.allowed-callers]
                set_reset_reason = ["sys"]
            "#},
            indoc! {r#"
                [tasks.jefe]
                config.do-not-dump = ["chaos"]
            "#},
            indoc! {r#"
                [tasks.jefe]
                features = ["hello", "world"]

                [tasks.jefe.config]
                do-not-dump = ["chaos"]

                [tasks.jefe.config.allowed-callers]
                set_reset_reason = ["sys"]
            "#},
        );
    }
}
