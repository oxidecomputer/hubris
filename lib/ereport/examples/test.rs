// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use ereport::EreportData;

#[derive(Debug, EreportData)]
pub enum TestEnum {
    Variant1,
    #[ereport(rename = "hw.foo.cool-ereport-class")]
    Variant2,
}

#[derive(EreportData)]
struct TestStruct {
    #[ereport(rename = "a")]
    field1: u32,
    field2: TestEnum,
}

#[derive(EreportData)]
struct TestStruct2<D> {
    #[ereport(skip_if_nil)]
    field6: Option<bool>,
    #[ereport(flatten)]
    inner: D,
}

fn main() {
    println!("TestEnum::MAX_CBOR_LEN = {}", TestEnum::MAX_CBOR_LEN);
    println!("TestStruct::MAX_CBOR_LEN = {}", TestStruct::MAX_CBOR_LEN);

    println!(
        "TestStruct2::<TestStruct>::MAX_CBOR_LEN = {}",
        TestStruct2::<TestStruct>::MAX_CBOR_LEN
    );
}
