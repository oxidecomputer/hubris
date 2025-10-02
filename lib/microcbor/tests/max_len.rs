// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use microcbor::{Encode, EncodeFields, StaticCborLen};
use proptest::test_runner::TestCaseError;
use proptest_derive::Arbitrary;

#[derive(Debug, Encode, Arbitrary)]
pub enum TestEnum {
    Variant1,
    #[cbor(rename = "hw.foo.cool-cbor-class")]
    Variant2,
    #[cbor(rename = "hw.bar.baz.fault.computer_on_fire")]
    Variant3,
}

#[derive(Debug, Encode, EncodeFields, Arbitrary)]
struct TestStruct {
    #[cbor(rename = "a")]
    field1: u32,
    field2: TestEnum,
}

#[derive(Debug, Encode, EncodeFields, Arbitrary)]
struct TestStruct2<D> {
    #[cbor(skip_if_nil)]
    field6: Option<bool>,
    #[cbor(flatten)]
    inner: D,
}

#[derive(Debug, Encode, EncodeFields, Arbitrary)]
enum TestEnum2<D> {
    Flattened {
        #[cbor(flatten)]
        flattened: D,
        bar: u32,
    },
    Nested {
        nested: D,
        quux: f32,
    },
}

#[derive(Debug, Encode, Arbitrary)]
enum TestEnum3 {
    Named {
        field1: u32,
        tuple_struct: TestTupleStruct1,
    },
    Unnamed(u32, TestTupleStruct2),
    UnnamedSingle(f64),
}

#[derive(Debug, Encode, Arbitrary)]
struct TestTupleStruct1(u32, u64);

#[derive(Debug, Encode, Arbitrary)]
struct TestTupleStruct2(u64);

#[derive(Debug, Encode, Arbitrary)]
struct TestStructWithArrays {
    bytes: [u8; 10],
    enums: [TestEnum3; 4],
    blargh: usize,
}

#[derive(Debug, Encode, EncodeFields, Arbitrary)]
#[cbor(variant_id = "my_cool_tag")]
enum TestVariantIdEnum {
    #[cbor(rename = "renamed_fields_variant")]
    RenamedFieldsVariant {
        a: u32,
        b: u64,
    },
    FieldsVariant {
        c: u64,
        d: bool,
    },
    #[cbor(rename = "renamed_unit_variant")]
    RenamedUnitVariant,
    UnitVariant,
}

#[track_caller]
fn assert_max_len<T: StaticCborLen + std::fmt::Debug>(
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

    #[test]
    fn array(input: [TestStruct; 10]) {
        assert_max_len(&input)?;
    }

    #[test]
    fn struct_with_arrays(input: TestStructWithArrays) {
        assert_max_len(&input)?;
    }

    #[test]
    fn variant_id_enum(input: TestVariantIdEnum) {
        assert_max_len(&input)?;
    }

    #[test]
    fn variant_id_enum_nested_or_flattened(input: TestEnum2<TestVariantIdEnum>) {
        assert_max_len(&input)?;
    }

    #[test]
    fn variant_id_enum_array(input: [TestVariantIdEnum; 10]) {
        assert_max_len(&input)?;
    }
}
