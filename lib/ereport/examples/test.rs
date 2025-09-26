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

#[derive(Debug, EreportData)]
struct TestStruct {
    #[ereport(rename = "a")]
    field1: u32,
    field2: TestEnum,
}

#[derive(Debug, EreportData)]
struct TestStruct2<D> {
    #[ereport(skip_if_nil)]
    field6: Option<bool>,
    #[ereport(flatten)]
    inner: D,
}

#[derive(Debug, EreportData)]
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

fn main() {
    const MAX_LEN: usize = ereport::max_cbor_len_for! {
        TestEnum,
        TestStruct2<TestStruct>,
        TestEnum2<TestStruct>,
        TestStruct2<TestEnum2<TestStruct>>
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
}

fn test_one_type<T: EreportData + std::fmt::Debug>(input: T, buf: &mut [u8]) {
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
