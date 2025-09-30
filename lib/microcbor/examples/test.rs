// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use microcbor::{Encode, EncodeFields, StaticCborLen};

#[derive(Debug, Encode)]
pub enum TestEnum {
    Variant1,
    #[cbor(rename = "hw.foo.cool-ereport-class")]
    Variant2,
}

#[derive(Debug, Encode, EncodeFields)]
struct TestStruct {
    #[cbor(rename = "a")]
    field1: u32,
    field2: TestEnum,
}

#[derive(Debug, Encode)]
struct TestStruct2<D> {
    #[cbor(skip_if_nil)]
    field6: Option<bool>,
    #[cbor(flatten)]
    inner: D,
}

#[derive(Debug, Encode, EncodeFields)]
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

#[derive(Debug, Encode)]
enum TestEnum3 {
    Named {
        field1: u32,
        tuple_struct: TestTupleStruct1,
    },
    Unnamed(u32, TestTupleStruct2),
    UnnamedSingle(f64),
}

#[derive(Debug, Encode)]
struct TestTupleStruct1(u32, u64);

#[derive(Debug, Encode)]
struct TestTupleStruct2(u64);

fn main() {
    const MAX_LEN: usize = microcbor::max_cbor_len_for! {
        TestEnum,
        TestStruct2<TestStruct>,
        TestEnum2<TestStruct>,
        TestStruct2<TestEnum2<TestStruct>>,
        TestEnum3,
    };
    let mut buf = [0u8; MAX_LEN];

    test_one_type(TestEnum::Variant2, &mut buf);
    test_one_type(
        TestStruct {
            field1: 32,
            field2: TestEnum::Variant1,
        },
        &mut buf,
    );
    test_one_type(
        TestStruct2 {
            field6: None,
            inner: TestStruct {
                field1: 4,
                field2: TestEnum::Variant1,
            },
        },
        &mut buf,
    );
    test_one_type(
        TestEnum2::Nested {
            nested: TestStruct {
                field1: 8,
                field2: TestEnum::Variant2,
            },
            quux: 10.6,
        },
        &mut buf,
    );
    test_one_type(
        TestEnum2::Flattened {
            flattened: TestStruct {
                field1: 8,
                field2: TestEnum::Variant2,
            },
            bar: 10,
        },
        &mut buf,
    );

    test_one_type(
        TestStruct2 {
            field6: Some(true),
            inner: TestEnum2::Nested {
                nested: TestStruct {
                    field1: 16,
                    field2: TestEnum::Variant1,
                },
                quux: 87.666,
            },
        },
        &mut buf,
    );

    test_one_type(
        TestEnum3::Named {
            field1: 69,
            tuple_struct: TestTupleStruct1(1, 2),
        },
        &mut buf,
    );

    test_one_type(
        TestEnum3::Unnamed(420, TestTupleStruct2(0xc0ffee)),
        &mut buf,
    );

    test_one_type(TestEnum3::UnnamedSingle(42069.0), &mut buf);
}

fn test_one_type<T: StaticCborLen + std::fmt::Debug>(input: T, buf: &mut [u8]) {
    println!(
        "{}::MAX_CBOR_LEN = {}",
        std::any::type_name::<T>(),
        T::MAX_CBOR_LEN
    );

    println!("value = {input:?}");
    let cursor = minicbor::encode::write::Cursor::new(buf);
    let mut encoder = minicbor::encode::Encoder::new(cursor);
    encoder.encode(&input).unwrap();
    let cursor = encoder.into_writer();
    let len: usize = cursor.position();
    let buf = &cursor.into_inner()[..len];
    println!("value.encode.len() = {len}");

    println!("CBOR = {}", minicbor::display(buf));
    println!();
}
