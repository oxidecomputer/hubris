#![feature(prelude_import)]
#[prelude_import]
use std::prelude::rust_2024::*;
#[macro_use]
extern crate std;
use ereport::EreportData;
pub enum TestEnum {
    Variant1,
    #[ereport(rename = "hw.foo.cool-ereport-class")]
    Variant2,
}
#[automatically_derived]
impl ::core::fmt::Debug for TestEnum {
    #[inline]
    fn fmt(&self, f: &mut ::core::fmt::Formatter) -> ::core::fmt::Result {
        ::core::fmt::Formatter::write_str(
            f,
            match self {
                TestEnum::Variant1 => "Variant1",
                TestEnum::Variant2 => "Variant2",
            },
        )
    }
}
#[automatically_derived]
impl ::ereport::EreportData for TestEnum {
    const MAX_CBOR_LEN: usize = {
        let mut max = 0;
        if ::ereport::str_cbor_len("Variant1") > max {
            max = ::ereport::str_cbor_len("Variant1");
        }
        if ::ereport::str_cbor_len("hw.foo.cool-ereport-class") > max {
            max = ::ereport::str_cbor_len("hw.foo.cool-ereport-class");
        }
        max
    };
}
impl<C> ::ereport::encode::Encode<C> for TestEnum {
    fn encode<W: ::ereport::encode::Write>(
        &self,
        e: &mut ::ereport::encode::Encoder<W>,
        _: &mut C,
    ) -> Result<(), ::ereport::encode::Error<W::Error>> {
        match self {
            TestEnum::Variant1 => e.str("Variant1")?,
            TestEnum::Variant2 => e.str("hw.foo.cool-ereport-class")?,
        }
        Ok(())
    }
}
struct TestStruct {
    #[ereport(rename = "a")]
    field1: u32,
    field2: TestEnum,
}
#[automatically_derived]
impl ::ereport::EreportData for TestStruct
where
    u32: ::ereport::EreportData,
    TestEnum: ::ereport::EreportData,
{
    const MAX_CBOR_LEN: usize =
        2 + <Self as ::ereport::EncodeFields<()>>::MAX_FIELDS_LEN;
}
#[automatically_derived]
impl<C> ::ereport::encode::Encode<C> for TestStruct
where
    u32: ::ereport::EreportData + ::ereport::Encode<C>,
    TestEnum: ::ereport::EreportData + ::ereport::Encode<C>,
{
    fn encode<W: ::ereport::encode::Write>(
        &self,
        e: &mut ::ereport::encode::Encoder<W>,
        c: &mut C,
    ) -> Result<(), ::ereport::encode::Error<W::Error>> {
        e.begin_map()?;
        <Self as ::ereport::EncodeFields<C>>::encode_fields(self, e, c)?;
        e.end()?;
        Ok(())
    }
}
#[automatically_derived]
impl<C> ::ereport::EncodeFields<C> for TestStruct
where
    u32: ::ereport::EreportData + ::ereport::Encode<C>,
    TestEnum: ::ereport::EreportData + ::ereport::Encode<C>,
{
    const MAX_FIELDS_LEN: usize = {
        let mut len = 0;
        len += ::ereport::str_cbor_len("a");
        len += <u32 as ::ereport::EreportData>::MAX_CBOR_LEN;
        len += ::ereport::str_cbor_len("field2");
        len += <TestEnum as ::ereport::EreportData>::MAX_CBOR_LEN;
        len
    };
    fn encode_fields<W: ::ereport::encode::Write>(
        &self,
        e: &mut ::ereport::encode::Encoder<W>,
        c: &mut C,
    ) -> Result<(), ::ereport::encode::Error<W::Error>> {
        e.str("a")?;
        ::ereport::Encode::<C>::encode(&self.field1, e, c)?;
        e.str("field2")?;
        ::ereport::Encode::<C>::encode(&self.field2, e, c)?;
        Ok(())
    }
}
fn main() {
    // {
    //     ::std::io::_print(format_args!(
    //         "TestEnum::MAX_CBOR_LEN = {0}\n",
    //         TestEnum::MAX_CBOR_LEN
    //     ));
    // };
    // {
    //     ::std::io::_print(format_args!(
    //         "TestStruct::MAX_CBOR_LEN = {0}\n",
    //         TestStruct::MAX_CBOR_LEN
    //     ));
    // };
}
