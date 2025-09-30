// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use ereport::EreportData;
use proptest::test_runner::TestCaseError;
use proptest_derive::Arbitrary;

#[derive(Debug, EreportData, Arbitrary)]
pub enum TestEnum {
    Variant1,
    #[ereport(rename = "hw.foo.cool-ereport-class")]
    Variant2,
    #[ereport(rename = "hw.bar.baz.fault.computer_on_fire")]
    Variant3,
}

#[derive(Debug, EreportData, Arbitrary)]
struct TestStruct {
    #[ereport(rename = "a")]
    field1: u32,
    field2: TestEnum,
}

#[derive(Debug, EreportData, Arbitrary)]
struct TestStruct2<D> {
    #[ereport(skip_if_nil)]
    field6: Option<bool>,
    #[ereport(flatten)]
    inner: D,
}

#[derive(Debug, EreportData, Arbitrary)]
enum TestEnum2<D> {
    Flattened {
        #[ereport(flatten)]
        flattened: D,
        bar: u32,
    },
    Nested {
        nested: D,
        quux: f32,
    },
}

#[derive(Debug, EreportData, Arbitrary)]
enum TestEnum3 {
    Named {
        field1: u32,
        tuple_struct: TestTupleStruct1,
    },
    Unnamed(u32, TestTupleStruct2),
    UnnamedSingle(f64),
}

#[derive(Debug, EreportData, Arbitrary)]
struct TestTupleStruct1(u32, u64);

#[derive(Debug, EreportData, Arbitrary)]
struct TestTupleStruct2(u64);

#[track_caller]
fn assert_max_len<T: EreportData + std::fmt::Debug>(
    input: &T,
) -> Result<(), TestCaseError> {
    let max_size = T::MAX_CBOR_LEN;
    let encoded = match minicbor::to_vec(input) {
        Ok(bytes) => bytes,
        Err(err) => {
            return Err(TestCaseError::fail(format!(
                "input did not encode: {err}\ninput = {input:#?}"
            )));
        }
    };
    proptest::prop_assert!(
        encoded.len() <= max_size,
        "encoded representation's length of {}B exceeded alleged max size \
         of {max_size}B\n\
         input: {input:#?}\n\
         encoded: {}",
        encoded.len(),
        minicbor::display(&encoded),
    );
    Ok(())
}

proptest::proptest! {
    #[test]
    fn flattened_enum_variant(input: TestEnum2<TestStruct2<TestStruct>>) {
        assert_max_len(&input)?;
    }

    #[test]
    fn enum_tuple_structs(input: TestEnum3) {
        assert_max_len(&input)?;
    }

    #[test]
    fn unflattened_struct(input: TestStruct) {
        assert_max_len(&input)?;
    }
}
