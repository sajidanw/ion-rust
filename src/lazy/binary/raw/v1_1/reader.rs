#![allow(non_camel_case_types)]

use crate::lazy::binary::raw::v1_1::immutable_buffer::ImmutableBuffer;
use crate::lazy::binary::raw::v1_1::value::LazyRawBinaryValue_1_1;
use crate::lazy::decoder::{LazyDecoder, LazyRawReader, RawVersionMarker};
use crate::lazy::encoder::private::Sealed;
use crate::lazy::encoding::BinaryEncoding_1_1;
use crate::lazy::raw_stream_item::{EndPosition, LazyRawStreamItem, RawStreamItem};
use crate::result::IonFailure;
use crate::IonResult;

use bumpalo::Bump as BumpAllocator;

pub struct LazyRawBinaryReader_1_1<'data> {
    data: ImmutableBuffer<'data>,
    bytes_to_skip: usize, // Bytes to skip in order to advance to the next item.
}

impl<'data> LazyRawBinaryReader_1_1<'data> {
    fn new(data: &'data [u8]) -> Self {
        Self::new_with_offset(data, 0)
    }

    fn new_with_offset(data: &'data [u8], offset: usize) -> Self {
        let data = ImmutableBuffer::new_with_offset(data, offset);
        Self {
            data,
            bytes_to_skip: 0,
        }
    }

    fn read_ivm<'top>(
        &mut self,
        buffer: ImmutableBuffer<'data>,
    ) -> IonResult<LazyRawStreamItem<'top, BinaryEncoding_1_1>>
    where
        'data: 'top,
    {
        let (marker, _buffer_after_ivm) = buffer.read_ivm()?;
        let (major, minor) = marker.version();
        if (major, minor) != (1, 1) {
            return IonResult::decoding_error(format!(
                "unsupported version of Ion: v{major}.{minor}; only 1.1 is supported by this reader",
            ));
        }
        self.data = buffer;
        self.bytes_to_skip = 4;
        Ok(LazyRawStreamItem::<BinaryEncoding_1_1>::VersionMarker(
            marker,
        ))
    }

    fn read_value<'top>(
        &mut self,
        buffer: ImmutableBuffer<'data>,
    ) -> IonResult<LazyRawStreamItem<'top, BinaryEncoding_1_1>>
    where
        'data: 'top,
    {
        let lazy_value = match ImmutableBuffer::peek_sequence_value(buffer)? {
            Some(lazy_value) => lazy_value,
            None => {
                return Ok(LazyRawStreamItem::<BinaryEncoding_1_1>::EndOfStream(
                    EndPosition::new(self.position()),
                ))
            }
        };
        self.data = buffer;
        self.bytes_to_skip = lazy_value.encoded_value.total_length();
        Ok(RawStreamItem::Value(lazy_value))
    }

    fn advance_to_next_item(&self) -> IonResult<ImmutableBuffer<'data>> {
        if self.data.len() < self.bytes_to_skip {
            return IonResult::incomplete(
                "cannot advance to next item, insufficient data in buffer",
                self.data.offset(),
            );
        }

        if self.bytes_to_skip > 0 {
            Ok(self.data.consume(self.bytes_to_skip))
        } else {
            Ok(self.data)
        }
    }

    pub fn next<'top>(&'top mut self) -> IonResult<LazyRawStreamItem<'top, BinaryEncoding_1_1>>
    where
        'data: 'top,
    {
        let mut buffer = self.advance_to_next_item()?;
        if buffer.is_empty() {
            return Ok(LazyRawStreamItem::<BinaryEncoding_1_1>::EndOfStream(
                EndPosition::new(buffer.offset()),
            ));
        }

        let type_descriptor = buffer.peek_opcode()?;
        if type_descriptor.is_nop() {
            (_, buffer) = buffer.consume_nop_padding(type_descriptor)?;
            if buffer.is_empty() {
                return Ok(LazyRawStreamItem::<BinaryEncoding_1_1>::EndOfStream(
                    EndPosition::new(buffer.offset()),
                ));
            }
        }
        if type_descriptor.is_ivm_start() {
            return self.read_ivm(buffer);
        }
        self.read_value(buffer)
    }

    /// Runs the provided parsing function on this reader's buffer.
    /// If it succeeds, marks the reader  as ready to advance by the 'n' bytes
    /// that were consumed.
    /// If it does not succeed, the `DataSource` remains unchanged.
    pub(crate) fn try_parse_next<
        F: Fn(ImmutableBuffer) -> IonResult<Option<LazyRawBinaryValue_1_1<'data>>>,
    >(
        &mut self,
        parser: F,
    ) -> IonResult<Option<LazyRawBinaryValue_1_1<'data>>> {
        let buffer = self.advance_to_next_item()?;

        let lazy_value = match parser(buffer) {
            Ok(Some(output)) => output,
            Ok(None) => return Ok(None),
            Err(e) => return Err(e),
        };

        // If the value we read doesn't start where we began reading, there was a NOP.
        // let num_nop_bytes = lazy_value.input.offset() - buffer.offset();
        self.bytes_to_skip = lazy_value.encoded_value.total_length();
        Ok(Some(lazy_value))
    }
}

impl<'data> Sealed for LazyRawBinaryReader_1_1<'data> {}

impl<'data> LazyRawReader<'data, BinaryEncoding_1_1> for LazyRawBinaryReader_1_1<'data> {
    fn new(data: &'data [u8]) -> Self {
        Self::new(data)
    }

    fn next<'top>(
        &'top mut self,
        _allocator: &'top BumpAllocator,
    ) -> IonResult<LazyRawStreamItem<'top, BinaryEncoding_1_1>>
    where
        'data: 'top,
    {
        self.next()
    }

    fn resume_at_offset(
        data: &'data [u8],
        offset: usize,
        _saved_state: <BinaryEncoding_1_1 as LazyDecoder>::ReaderSavedState,
    ) -> Self {
        Self::new_with_offset(data, offset)
    }

    fn position(&self) -> usize {
        self.data.offset() + self.bytes_to_skip
    }
}

#[cfg(test)]
mod tests {
    use crate::lazy::binary::raw::v1_1::reader::LazyRawBinaryReader_1_1;
    use crate::{IonResult, IonType};
    use rstest::*;

    #[test]
    fn nop() -> IonResult<()> {
        let data: Vec<u8> = vec![
            0xE0, 0x01, 0x01, 0xEA, // IVM
            0xEC, // 1-Byte NOP
            0xEC, 0xEC, // 2-Byte NOP
            0xEC, 0xEC, 0xEC, // 3-Byte Nop
            0xED, 0x05, 0x00, 0x00, // 4-Byte NOP
            0xEA, // null.null
        ];

        let mut reader = LazyRawBinaryReader_1_1::new(&data);
        let _ivm = reader.next()?.expect_ivm()?;

        assert_eq!(
            reader.next()?.expect_value()?.read()?.expect_null()?,
            IonType::Null
        );

        Ok(())
    }

    #[test]
    fn bools() -> IonResult<()> {
        let data: Vec<u8> = vec![
            0xE0, 0x01, 0x01, 0xEA, // IVM
            0x5E, // true
            0x5F, // false
        ];

        let mut reader = LazyRawBinaryReader_1_1::new(&data);
        let _ivm = reader.next()?.expect_ivm()?;

        assert!(reader.next()?.expect_value()?.read()?.expect_bool()?);

        assert!(!(reader.next()?.expect_value()?.read()?.expect_bool()?));

        Ok(())
    }

    #[test]
    fn integers() -> IonResult<()> {
        #[rustfmt::skip]
        let data: Vec<u8> = vec![
            // IVM
            0xE0, 0x01, 0x01, 0xEA,

            // Integer: 0
            0x50,

            // Integer: 17
            0x51, 0x11,

            // Integer: -944
            0x52, 0x50, 0xFC,

            // Integer: 1
            0xF5, 0x03, 0x01,

            // Integer: 147573952589676412929
            0xF5, 0x13, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x08,
        ];

        let mut reader = LazyRawBinaryReader_1_1::new(&data);
        let _ivm = reader.next()?.expect_ivm()?;

        assert_eq!(
            reader.next()?.expect_value()?.read()?.expect_int()?,
            0.into()
        );
        assert_eq!(
            reader.next()?.expect_value()?.read()?.expect_int()?,
            17.into()
        );
        assert_eq!(
            reader.next()?.expect_value()?.read()?.expect_int()?,
            (-944).into()
        );

        assert_eq!(
            reader.next()?.expect_value()?.read()?.expect_int()?,
            1.into()
        );

        assert_eq!(
            reader.next()?.expect_value()?.read()?.expect_int()?,
            147573952589676412929i128.into()
        );
        Ok(())
    }

    #[test]
    fn strings() -> IonResult<()> {
        #[rustfmt::skip]
        let data: Vec<u8> = vec![
            // IVM
            0xe0, 0x01, 0x01, 0xea,

            // String: ""
            0x80,

            // String: "hello"
            0x85, 0x68, 0x65, 0x6c, 0x6c, 0x6f,

            // String: "fourteen bytes"
            0x8E, 0x66, 0x6F, 0x75, 0x72, 0x74, 0x65, 0x65, 0x6E, 0x20, 0x62, 0x79, 0x74, 0x65,
            0x73,

            // String: "variable length encoding"
            0xF8, 0x31, 0x76, 0x61, 0x72, 0x69, 0x61, 0x62, 0x6C, 0x65, 0x20, 0x6C, 0x65,
            0x6E, 0x67, 0x74, 0x68, 0x20, 0x65, 0x6E, 0x63, 0x6f, 0x64, 0x69, 0x6E, 0x67,
        ];

        let mut reader = LazyRawBinaryReader_1_1::new(&data);
        let _ivm = reader.next()?.expect_ivm()?;

        assert_eq!(reader.next()?.expect_value()?.read()?.expect_string()?, "");

        assert_eq!(
            reader.next()?.expect_value()?.read()?.expect_string()?,
            "hello"
        );

        assert_eq!(
            reader.next()?.expect_value()?.read()?.expect_string()?,
            "fourteen bytes"
        );

        assert_eq!(
            reader.next()?.expect_value()?.read()?.expect_string()?,
            "variable length encoding"
        );

        Ok(())
    }

    #[test]
    fn symbols() -> IonResult<()> {
        use crate::RawSymbolRef;

        #[rustfmt::skip]
        let data: Vec<u8> = vec![
            // IVM
            0xE0, 0x01, 0x01, 0xEA,

            // Symbol: ''
            0x90,

            // Symbol: 'fourteen bytes'
            0x9E, 0x66, 0x6F, 0x75, 0x72, 0x74, 0x65, 0x65, 0x6E, 0x20, 0x62, 0x79, 0x74, 0x65,
            0x73,

            // Symbol: 'variable length encoding'
            0xF9, 0x31, 0x76, 0x61, 0x72, 0x69, 0x61, 0x62, 0x6C, 0x65, 0x20, 0x6C, 0x65, 0x6E,
            0x67, 0x74, 0x68, 0x20, 0x65, 0x6E, 0x63, 0x6f, 0x64, 0x69, 0x6E, 0x67,

            // Symbol ID: 1
            0xE1, 0x01,

            // Symbol ID: 257
            0xE2, 0x01, 0x00,

            // Symbol ID: 65,793
            0xE3, 0x01, 0x00, 0x00,
        ];

        let mut reader = LazyRawBinaryReader_1_1::new(&data);
        let _ivm = reader.next()?.expect_ivm()?;

        assert_eq!(
            reader.next()?.expect_value()?.read()?.expect_symbol()?,
            "".into()
        );

        assert_eq!(
            reader.next()?.expect_value()?.read()?.expect_symbol()?,
            "fourteen bytes".into()
        );

        assert_eq!(
            reader.next()?.expect_value()?.read()?.expect_symbol()?,
            "variable length encoding".into()
        );

        assert_eq!(
            reader.next()?.expect_value()?.read()?.expect_symbol()?,
            RawSymbolRef::SymbolId(1)
        );

        assert_eq!(
            reader.next()?.expect_value()?.read()?.expect_symbol()?,
            RawSymbolRef::SymbolId(257)
        );

        assert_eq!(
            reader.next()?.expect_value()?.read()?.expect_symbol()?,
            RawSymbolRef::SymbolId(65793)
        );

        Ok(())
    }

    #[test]
    #[allow(clippy::approx_constant)]
    fn floats() -> IonResult<()> {
        #[rustfmt::skip]
        let data: Vec<u8> = vec![
            // IVM
            0xe0, 0x01, 0x01, 0xea,
            // 0e0
            0x5A,

            // 3.14 (half-precision)
            // 0x5B, 0x42, 0x47,

            // 3.1415927 (single-precision)
            0x5C, 0xdb, 0x0F, 0x49, 0x40,

            // 3.141592653589793 (double-precision)
            0x5D, 0x18, 0x2D, 0x44, 0x54, 0xFB, 0x21, 0x09, 0x40,
        ];

        let mut reader = LazyRawBinaryReader_1_1::new(&data);
        let _ivm = reader.next()?.expect_ivm()?;

        assert_eq!(reader.next()?.expect_value()?.read()?.expect_float()?, 0.0);

        // TODO: Implement Half-precision.
        // assert_eq!(reader.next()?.expect_value()?.read()?.expect_float()?, 3.14);

        assert_eq!(
            reader.next()?.expect_value()?.read()?.expect_float()? as f32,
            3.1415927f32,
        );

        assert_eq!(
            reader.next()?.expect_value()?.read()?.expect_float()?,
            std::f64::consts::PI,
        );

        Ok(())
    }

    #[rstest]
    #[case("0.", &[0x60])]
    #[case("0d1", &[0x61, 0x03])]
    #[case("0d63", &[0x61, 0x7F])]
    #[case("0d64", &[0x62, 0x02, 0x01])]
    #[case("0d99", &[0x62, 0x8E, 0x01])]
    #[case("0.0", &[0x61, 0xFF])]
    #[case("0.00", &[0x61, 0xFD])]
    #[case("0.000", &[0x61, 0xFB])]
    #[case("0d-64", &[0x61, 0x81])]
    #[case("0d-99", &[0x62, 0x76, 0xFE])]
    #[case("-0.", &[0x62, 0x01, 0x00])]
    #[case("-0d1", &[0x62, 0x03, 0x00])]
    #[case("-0d3", &[0x62, 0x07, 0x00])]
    #[case("-0d63", &[0x62, 0x7F, 0x00])]
    #[case("-0d199", &[0x63, 0x1E, 0x03, 0x00])]
    #[case("-0d-1", &[0x62, 0xFF, 0x00])]
    #[case("-0d-2", &[0x62, 0xFD, 0x00])]
    #[case("-0d-3", &[0x62, 0xFB, 0x00])]
    #[case("-0d-63", &[0x62, 0x83, 0x00])]
    #[case("-0d-64", &[0x62, 0x81, 0x00])]
    #[case("-0d-65", &[0x63, 0xFE, 0xFE, 0x00])]
    #[case("-0d-199", &[0x63, 0xE6, 0xFC, 0x00])]
    #[case("0.01", &[0x62, 0xFD, 0x01])]
    #[case("0.1", &[0x62, 0xFF, 0x01])]
    #[case("1d0", &[0x62, 0x01, 0x01])]
    #[case("1d1", &[0x62, 0x03, 0x01])]
    #[case("1d2", &[0x62, 0x05, 0x01])]
    #[case("1d63", &[0x62, 0x7F, 0x01])]
    #[case("1d64", &[0x63, 0x02, 0x01, 0x01])]
    #[case("1d65536", &[0x64, 0x04, 0x00, 0x08, 0x01])]
    #[case("2.", &[0x62, 0x01, 0x02])]
    #[case("7.", &[0x62, 0x01, 0x07])]
    #[case("14d0", &[0x62, 0x01, 0x0E])]
    #[case("14d0", &[0x63, 0x02, 0x00, 0x0E])] // overpadded exponent
    #[case("14d0", &[0x64, 0x01, 0x0E, 0x00, 0x00])] // Overpadded coefficient
    #[case("14d0", &[0x65, 0x02, 0x00, 0x0E, 0x00, 0x00])] // Overpadded coefficient and exponent
    #[case("1.0", &[0x62, 0xFF, 0x0A])]
    #[case("1.00", &[0x62, 0xFD, 0x64])]
    #[case("1.27", &[0x62, 0xFD, 0x7F])]
    #[case("1.28", &[0x63, 0xFD, 0x80, 0x00])]
    #[case("3.142", &[0x63, 0xFB, 0x46, 0x0C])]
    #[case("3.14159", &[0x64, 0xF7, 0x2F, 0xCB, 0x04])]
    #[case("3.1415927", &[0x65, 0xF3, 0x77, 0x5E, 0xDF, 0x01])]
    #[case("3.141592653", &[0x66, 0xEF, 0x4D, 0xE6, 0x40, 0xBB, 0x00])]
    #[case("3.141592653590", &[0x67, 0xE9, 0x16, 0x9F, 0x83, 0x75, 0xDB, 0x02])]
    #[case("3.14159265358979323", &[0x69, 0xDF, 0xFB, 0xA0, 0x9E, 0xF6, 0x2F, 0x1E, 0x5C, 0x04])]
    #[case("3.1415926535897932384626", &[0x6B, 0xD5, 0x72, 0x49, 0x64, 0xCC, 0xAF, 0xEF, 0x8F, 0x0F, 0xA7, 0x06])]
    #[case("3.141592653589793238462643383", &[0x6D, 0xCB, 0xB7, 0x3C, 0x92, 0x86, 0x40, 0x9F, 0x1B, 0x01, 0x1F, 0xAA, 0x26, 0x0A])]
    #[case("3.14159265358979323846264338327950", &[0x6F, 0xC1, 0x8E, 0x29, 0xE5, 0xE3, 0x56, 0xD5, 0xDF, 0xC5, 0x10, 0x8F, 0x55, 0x3F, 0x7D, 0x0F])]
    #[case("3.141592653589793238462643383279503", &[0xF6, 0x21, 0xBF, 0x8F, 0x9F, 0xF3, 0xE6, 0x64, 0x55, 0xBE, 0xBA, 0xA7, 0x96, 0x57, 0x79, 0xE4, 0x9A, 0x00])]
    fn decimals(#[case] expected_txt: &str, #[case] ion_data: &[u8]) -> IonResult<()> {
        use crate::lazy::decoder::{LazyRawReader, LazyRawValue};
        use crate::lazy::text::raw::v1_1::reader::LazyRawTextReader_1_1;
        let bump = bumpalo::Bump::new();

        let mut reader_txt = LazyRawTextReader_1_1::new(expected_txt.as_bytes());
        let mut reader_bin = LazyRawBinaryReader_1_1::new(ion_data);

        assert_eq!(
            reader_bin
                .next()?
                .expect_value()?
                .read()?
                .expect_decimal()?,
            reader_txt
                .next(&bump)?
                .expect_value()?
                .read()?
                .expect_decimal()?,
        );
        Ok(())
    }

    #[rstest]
    #[case("0.", &[0xF6, 0x01])]
    #[case("0d99", &[0xF6, 0x05, 0x8E, 0x01])]
    #[case("0.0", &[0xF6, 0x03, 0xFF])]
    #[case("0.00", &[0xF6, 0x03, 0xFD])]
    #[case("0d-99", &[0xF6, 0x05, 0x76, 0xFE])]
    #[case("-0.", &[0xF6, 0x05, 0x01, 0x00])]
    #[case("-0d199", &[0xF6, 0x07, 0x1E, 0x03, 0x00])]
    #[case("-0d-1", &[0xF6, 0x05, 0xFF, 0x00])]
    #[case("-0d-65", &[0xF6, 0x07, 0xFE, 0xFE, 0x00])]
    #[case("0.01", &[0xF6, 0x05, 0xFD, 0x01])]
    #[case("1.", &[0xF6, 0x05, 0x01, 0x01])]
    #[case("1d65536", &[0xF6, 0x09, 0x04, 0x00, 0x08, 0x01])]
    #[case("1.0", &[0xF6, 0x05, 0xFF, 0x0A])]
    #[case("1.28", &[0xF6, 0x07, 0xFD, 0x80, 0x00])]
    #[case("3.141592653590", &[0xF6, 0x0F, 0xE9, 0x16, 0x9F, 0x83, 0x75, 0xDB, 0x02])]
    #[case("3.14159265358979323", &[0xF6, 0x13, 0xDF, 0xFB, 0xA0, 0x9E, 0xF6, 0x2F, 0x1E, 0x5C, 0x04])]
    #[case("3.1415926535897932384626", &[0xF6, 0x17, 0xD5, 0x72, 0x49, 0x64, 0xCC, 0xAF, 0xEF, 0x8F, 0x0F, 0xA7, 0x06])]
    #[case("3.141592653589793238462643383", &[0xF6, 0x1B, 0xCB, 0xB7, 0x3C, 0x92, 0x86, 0x40, 0x9F, 0x1B, 0x01, 0x1F, 0xAA, 0x26, 0x0A])]
    #[case("3.14159265358979323846264338327950", &[0xF6, 0x1F, 0xC1, 0x8E, 0x29, 0xE5, 0xE3, 0x56, 0xD5, 0xDF, 0xC5, 0x10, 0x8F, 0x55, 0x3F, 0x7D, 0x0F])]
    fn decimals_long(#[case] expected_txt: &str, #[case] ion_data: &[u8]) -> IonResult<()> {
        use crate::ion_data::IonEq;
        use crate::lazy::decoder::{LazyRawReader, LazyRawValue};
        use crate::lazy::text::raw::v1_1::reader::LazyRawTextReader_1_1;
        let bump = bumpalo::Bump::new();

        let mut reader_txt = LazyRawTextReader_1_1::new(expected_txt.as_bytes());
        let mut reader_bin = LazyRawBinaryReader_1_1::new(ion_data);

        let expected_value = reader_txt.next(&bump)?.expect_value()?.read()?;
        let actual_value = reader_bin.next()?.expect_value()?.read()?;

        assert!(actual_value
            .expect_decimal()?
            .ion_eq(&expected_value.expect_decimal()?));

        Ok(())
    }

    fn blobs() -> IonResult<()> {
        let data: Vec<u8> = vec![
            0xe0, 0x01, 0x01, 0xea, // IVM
            0xFE, 0x31, 0x49, 0x20, 0x61, 0x70, 0x70, 0x6c, 0x61, 0x75, 0x64, 0x20, 0x79, 0x6f,
            0x75, 0x72, 0x20, 0x63, 0x75, 0x72, 0x69, 0x6f, 0x73, 0x69, 0x74, 0x79,
        ];

        let mut reader = LazyRawBinaryReader_1_1::new(&data);
        let _ivm = reader.next()?.expect_ivm()?;

        let bytes: &[u8] = &[
            0x49, 0x20, 0x61, 0x70, 0x70, 0x6c, 0x61, 0x75, 0x64, 0x20, 0x79, 0x6f, 0x75, 0x72,
            0x20, 0x63, 0x75, 0x72, 0x69, 0x6f, 0x73, 0x69, 0x74, 0x79,
        ];
        assert_eq!(reader.next()?.expect_value()?.read()?.expect_blob()?, bytes);

        Ok(())
    }

    #[test]
    fn clobs() -> IonResult<()> {
        let data: Vec<u8> = vec![
            0xe0, 0x01, 0x01, 0xea, // IVM
            0xFF, 0x31, 0x49, 0x20, 0x61, 0x70, 0x70, 0x6c, 0x61, 0x75, 0x64, 0x20, 0x79, 0x6f,
            0x75, 0x72, 0x20, 0x63, 0x75, 0x72, 0x69, 0x6f, 0x73, 0x69, 0x74, 0x79,
        ];

        let mut reader = LazyRawBinaryReader_1_1::new(&data);
        let _ivm = reader.next()?.expect_ivm()?;

        let bytes: &[u8] = &[
            0x49, 0x20, 0x61, 0x70, 0x70, 0x6c, 0x61, 0x75, 0x64, 0x20, 0x79, 0x6f, 0x75, 0x72,
            0x20, 0x63, 0x75, 0x72, 0x69, 0x6f, 0x73, 0x69, 0x74, 0x79,
        ];

        assert_eq!(reader.next()?.expect_value()?.read()?.expect_clob()?, bytes);

        Ok(())
    }

    #[test]
    fn lists() -> IonResult<()> {
        use crate::lazy::decoder::LazyRawSequence;

        #[rustfmt::skip]
        let tests: &[(&[u8], &[IonType])] = &[
            // []
            (&[0xA0], &[]),

            // [null.null]
            (&[0xA1, 0xEA], &[IonType::Null]),

            // ['']
            (&[0xA1, 0x90], &[IonType::Symbol]),

            // ["hello"]
            (
                &[0xA6, 0x85, 0x68, 0x65, 0x6C, 0x6C, 0x6F],
                &[IonType::String],
            ),

            // [null.null, '', "hello"]
            (
                &[0xA8, 0xEA, 0x90, 0x85, 0x68, 0x65, 0x6C, 0x6c, 0x6F],
                &[IonType::Null, IonType::Symbol, IonType::String],
            ),

            // [3.1415927e0 3.1415927e0]
            (
                &[0xAA, 0x5C, 0xDB, 0x0F, 0x49, 0x40, 0x5C, 0xDB, 0x0F, 0x49, 0x40],
                &[IonType::Float, IonType::Float]
            ),

            // Long List Encoding

            // []
            (&[0xFA, 0x01], &[]),

            // ["variable length list"]
            (
                &[
                    0xFA, 0x2D, 0xF8, 0x29, 0x76, 0x61, 0x72, 0x69, 0x61, 0x62, 0x6C, 0x65,
                    0x20, 0x6C, 0x65, 0x6E, 0x67, 0x74, 0x68, 0x20, 0x6C, 0x69, 0x73, 0x74,
                ],
                &[IonType::String]
            ),

            // [<nop>]
            (&[0xFA, 0x03, 0xEC], &[]),
        ];

        for (ion_data, expected_types) in tests {
            let mut reader = LazyRawBinaryReader_1_1::new(ion_data);
            let container = reader.next()?.expect_value()?.read()?.expect_list()?;
            let mut count = 0;
            for (actual_lazy_value, expected_type) in container.iter().zip(expected_types.iter()) {
                let value = actual_lazy_value?.expect_value()?;
                assert_eq!(value.ion_type(), *expected_type);
                count += 1;
            }
            assert_eq!(count, expected_types.len());
        }

        Ok(())
    }

    #[test]
    fn sexp() -> IonResult<()> {
        use crate::lazy::decoder::LazyRawSequence;

        #[rustfmt::skip]
        let tests: &[(&[u8], &[IonType])] = &[
            // ()
            (&[0xB0], &[]),

            // (1 2 3)
            (
                &[0xB6, 0x51, 0x01, 0x51, 0x02, 0x51, 0x03],
                &[IonType::Int, IonType::Int, IonType::Int],
            ),

            // Long S-Expression Encoding

            // ()
            (&[0xFB, 0x01], &[]),

            // ("variable length sexp")
            (
                &[
                    0xFB, 0x2D, 0xF8, 0x29, 0x76, 0x61, 0x72, 0x69, 0x61, 0x62, 0x6C, 0x65, 0x20,
                    0x6C, 0x65, 0x6E, 0x67, 0x74, 0x68, 0x20, 0x73, 0x65, 0x78, 0x70
                ],
                &[IonType::String]
            ),

            // ( () () [] )
            (&[0xFB, 0x09, 0xFB, 0x01, 0xB0, 0xA0], &[IonType::SExp, IonType::SExp, IonType::List]),

            // ( $257 )
            (&[0xFB, 0x07, 0xE2, 0x01, 0x00], &[IonType::Symbol]),
        ];

        for (ion_data, expected_types) in tests {
            let mut reader = LazyRawBinaryReader_1_1::new(ion_data);
            let container = reader.next()?.expect_value()?.read()?.expect_sexp()?;
            let mut count = 0;
            for (actual_lazy_value, expected_type) in container.iter().zip(expected_types.iter()) {
                let value = actual_lazy_value?.expect_value()?;
                assert_eq!(value.ion_type(), *expected_type);
                count += 1;
            }
            assert_eq!(count, expected_types.len());
        }

        Ok(())
    }
}
