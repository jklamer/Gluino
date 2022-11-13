use core::slice;
use std::{
    io::Read,
    io::{self, Write},
};
use strum_macros::{EnumDiscriminants, EnumIter};

use crate::{
    compiled_spec::{CompiledSpec, SpecCompileError},
    util::{
        self, variable_length_decode_u64, variable_length_encode_u64, VariableLengthDecodingError,
    },
};

#[derive(Debug, Hash, Eq, PartialEq, Clone, EnumDiscriminants)]
#[strum_discriminants(name(SpecKind))]
#[strum_discriminants(derive(EnumIter))]
pub enum Spec {
    Bool,
    Uint(u8),
    Int(u8),
    BinaryFloatingPoint(InterchangeBinaryFloatingPointFormat),
    DecimalFloatingPoint(InterchangeDecimalFloatingPointFormat),
    Decimal {
        precision: u64,
        scale: u64,
    },
    Map {
        size: Size,
        key_spec: Box<Spec>,
        value_spec: Box<Spec>,
    },
    List {
        size: Size,
        value_spec: Box<Spec>,
    },
    String(Size, StringEncodingFmt),
    Bytes(Size),
    Optional(Box<Spec>),
    Name {
        name: String,
        spec: Box<Spec>,
    },
    Ref {
        name: String,
    },
    Record(Vec<(String, Spec)>),
    Tuple(Vec<Spec>),
    Enum(Vec<(String, Spec)>),
    Union(Vec<Spec>),
    Void,
}

//core
const BOOL: u8 = 32;
const UINT: u8 = 33;
const NAME: u8 = 34;
const INT: u8 = 35;
const BINARY_FP: u8 = 36;
const DECIMAL_FP: u8 = 37;
const REF: u8 = 38;
const VOID: u8 = 39;
const LIST: u8 = 40;
const MAP: u8 = 41;
const RECORD: u8 = 42;
const ENUM: u8 = 43;
const UNION: u8 = 45;
const DECIMAL: u8 = 46;
const TUPLE: u8 = 47;
const BYTES: u8 = 48;
const STRING: u8 = 49;
const OPTIONAL: u8 = 63;

// aliases
const UINT_0: u8 = 0;
const UINT_1: u8 = 1;
const UINT_2: u8 = 2;
const UINT_3: u8 = 3;
const INT_0: u8 = 4;
const INT_1: u8 = 5;
const INT_2: u8 = 6;
const INT_3: u8 = 7;
const SINGLE_FP: u8 = 8;
const DOUBLE_FP: u8 = 9;
const UTF8_STRING: u8 = 10;

impl Spec {
    pub fn compile(self) -> Result<CompiledSpec, SpecCompileError> {
        CompiledSpec::compile(self)
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(256);
        if let Err(e) = self.to_bytes_internal(&mut out) {
            panic!("{}", e.to_string())
        };
        out
    }

    pub(crate) fn to_longform_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(256);
        if let Err(e) = self.to_longform_bytes_internal(&mut out) {
            panic!("{}", e.to_string())
        };
        out
    }

    pub fn write_as_bytes<W: Write>(&self, w: &mut W) -> Result<usize, io::Error> {
        self.to_bytes_internal(w)
    }

    fn to_bytes_internal<W: Write>(&self, out: &mut W) -> Result<usize, io::Error> {
        Ok(match self {
            Spec::Bool => out.write(&[BOOL])?,
            Spec::Uint(scale) => match scale {
                0 => out.write(&[UINT_0])?,
                1 => out.write(&[UINT_1])?,
                2 => out.write(&[UINT_2])?,
                3 => out.write(&[UINT_3])?,
                s => out.write(&[UINT, *s])?,
            },
            Spec::Int(scale) => match scale {
                0 => out.write(&[INT_0])?,
                1 => out.write(&[INT_1])?,
                2 => out.write(&[INT_2])?,
                3 => out.write(&[INT_3])?,
                s => out.write(&[INT, *s])?,
            },
            Spec::BinaryFloatingPoint(fmt) => match fmt {
                InterchangeBinaryFloatingPointFormat::Single => out.write(&[SINGLE_FP])?,
                InterchangeBinaryFloatingPointFormat::Double => out.write(&[DOUBLE_FP])?,
                fmt => out.write(&[BINARY_FP])? + fmt.encode(out)?,
            },
            Spec::DecimalFloatingPoint(fmt) => out.write(&[DECIMAL_FP])? + fmt.encode(out)?,
            Spec::Decimal { precision, scale } => {
                out.write(&[DECIMAL])?
                    + variable_length_encode_u64(*precision, out)?
                    + variable_length_encode_u64(*scale, out)?
            }
            Spec::Map {
                size,
                key_spec,
                value_spec,
            } => {
                out.write(&[MAP])?
                    + size.encode(out)?
                    + Spec::to_bytes_internal(key_spec, out)?
                    + Spec::to_bytes_internal(value_spec, out)?
            }
            Spec::List { value_spec, size } => {
                out.write(&[LIST])? + size.encode(out)? + Spec::to_bytes_internal(value_spec, out)?
            }
            Spec::String(size, str_fmt) => {
                if matches!(size, Size::Variable) && matches!(str_fmt, StringEncodingFmt::Utf8) {
                    out.write(&[UTF8_STRING])?
                } else {
                    out.write(&[STRING])? + size.encode(out)? + str_fmt.encode(out)?
                }
            }
            Spec::Bytes(size) => out.write(&[BYTES])? + size.encode(out)?,
            Spec::Optional(optional_type) => {
                out.write(&[OPTIONAL])? + Spec::to_bytes_internal(&optional_type, out)?
            }
            Spec::Name { name, spec } => {
                out.write(&[NAME])?
                    + encode_string_utf8(name, out)?
                    + Spec::to_bytes_internal(spec, out)?
            }
            Spec::Ref { name } => out.write(&[REF])? + encode_string_utf8(name, out)?,
            Spec::Record(fields) => {
                out.write(&[RECORD])?
                    + variable_length_encode_u64(fields.len() as u64, out)?
                    + fields
                        .iter()
                        .map(|(name, spec)| {
                            combine(
                                encode_string_utf8(name, out),
                                Spec::to_bytes_internal(&spec, out),
                            )
                        })
                        .fold(Ok(0usize), combine)?
            }
            Spec::Tuple(fields) => {
                out.write(&[TUPLE])?
                    + variable_length_encode_u64(fields.len() as u64, out)?
                    + fields
                        .iter()
                        .map(|spec| Spec::to_bytes_internal(&spec, out))
                        .fold(Ok(0usize), combine)?
            }
            Spec::Enum(variants) => {
                out.write(&[ENUM])?
                    + variable_length_encode_u64(variants.len() as u64, out)?
                    + variants
                        .iter()
                        .map(|(name, spec)| {
                            combine(
                                encode_string_utf8(name, out),
                                Spec::to_bytes_internal(spec, out),
                            )
                        })
                        .fold(Ok(0usize), combine)?
            }
            Spec::Union(variants) => {
                out.write(&[UNION])?
                    + variable_length_encode_u64(variants.len() as u64, out)?
                    + variants
                        .iter()
                        .map(|spec| Spec::to_bytes_internal(spec, out))
                        .fold(Ok(0usize), combine)?
            }
            Spec::Void => out.write(&[VOID])?,
        })
    }

    pub fn read_from_bytes<R: Read>(input: &mut R) -> Result<Spec, SpecParsingError> {
        match next_byte(input)? {
            BOOL => Ok(Spec::Bool),
            VOID => Ok(Spec::Void),
            UINT => Ok(Spec::Uint(next_byte(input)?)),
            INT => Ok(Spec::Int(next_byte(input)?)),
            NAME => {
                let name = decode_utf8_string(input)?;
                let spec = Spec::read_from_bytes(input)?.into();
                Ok(Spec::Name { name, spec })
            }
            REF => Ok(Spec::Ref {
                name: decode_utf8_string(input)?,
            }),
            BINARY_FP => Ok(Spec::BinaryFloatingPoint(
                InterchangeBinaryFloatingPointFormat::decode(input)?,
            )),
            DECIMAL_FP => Ok(Spec::DecimalFloatingPoint(
                InterchangeDecimalFloatingPointFormat::decode(input)?,
            )),
            LIST => {
                let size = Size::decode(input)?;
                let value_spec = Spec::read_from_bytes(input)?.into();
                Ok(Spec::List { size, value_spec })
            }
            MAP => {
                let size = Size::decode(input)?;
                let key_spec = Spec::read_from_bytes(input)?.into();
                let value_spec = Spec::read_from_bytes(input)?.into();
                Ok(Spec::Map {
                    size,
                    key_spec,
                    value_spec,
                })
            }
            DECIMAL => {
                let precision = decode_u64(input)?;
                let scale = decode_u64(input)?;
                Ok(Spec::Decimal { precision, scale })
            }
            BYTES => {
                let size = Size::decode(input)?;
                Ok(Spec::Bytes(size))
            }
            STRING => {
                let size = Size::decode(input)?;
                let str_fmt = StringEncodingFmt::decode(input)?;
                Ok(Spec::String(size, str_fmt))
            }
            OPTIONAL => Ok(Spec::Optional(Spec::read_from_bytes(input)?.into())),
            RECORD => {
                let n = decode_u64(input)?;
                let mut v = Vec::with_capacity(n as usize);
                for _ in 0..n {
                    v.push((decode_utf8_string(input)?, Spec::read_from_bytes(input)?));
                }
                Ok(Spec::Record(v))
            }
            TUPLE => {
                let n = decode_u64(input)?;
                let mut v = Vec::with_capacity(n as usize);
                for _ in 0..n {
                    v.push(Spec::read_from_bytes(input)?);
                }
                Ok(Spec::Tuple(v))
            }
            ENUM => {
                let n = decode_u64(input)?;
                let mut v = Vec::with_capacity(n as usize);
                for _ in 0..n {
                    v.push((decode_utf8_string(input)?, Spec::read_from_bytes(input)?));
                }
                Ok(Spec::Enum(v))
            }
            UNION => {
                let n = decode_u64(input)?;
                let mut v = Vec::with_capacity(n as usize);
                for _ in 0..n {
                    v.push(Spec::read_from_bytes(input)?);
                }
                Ok(Spec::Union(v))
            }
            // aliases
            UINT_0 => Ok(Spec::Uint(0)),
            UINT_1 => Ok(Spec::Uint(1)),
            UINT_2 => Ok(Spec::Uint(2)),
            UINT_3 => Ok(Spec::Uint(3)),
            INT_0 => Ok(Spec::Int(0)),
            INT_1 => Ok(Spec::Int(1)),
            INT_2 => Ok(Spec::Int(2)),
            INT_3 => Ok(Spec::Int(3)),
            SINGLE_FP => Ok(Spec::BinaryFloatingPoint(
                InterchangeBinaryFloatingPointFormat::Single,
            )),
            DOUBLE_FP => Ok(Spec::BinaryFloatingPoint(
                InterchangeBinaryFloatingPointFormat::Double,
            )),
            UTF8_STRING => Ok(Spec::String(Size::Variable, StringEncodingFmt::Utf8)),
            flag => Err(SpecParsingError::UnknownSpecFlag(flag)),
        }
    }

    /// To longform bytes creates serialized view of spec without use of alias bytes for compression
    /// This byte representation is used for indentification purposes
    pub(crate) fn to_longform_bytes_internal<W: Write>(
        &self,
        out: &mut W,
    ) -> Result<usize, io::Error> {
        Ok(match self {
            Spec::Bool => out.write(&[BOOL])?,
            Spec::Uint(scale) => out.write(&[UINT, *scale])?,
            Spec::Int(scale) => out.write(&[INT, *scale])?,
            Spec::BinaryFloatingPoint(fmt) => out.write(&[BINARY_FP])? + fmt.encode(out)?,
            Spec::DecimalFloatingPoint(fmt) => out.write(&[DECIMAL_FP])? + fmt.encode(out)?,
            Spec::Decimal { precision, scale } => {
                out.write(&[DECIMAL])?
                    + variable_length_encode_u64(*precision, out)?
                    + variable_length_encode_u64(*scale, out)?
            }
            Spec::Map {
                size,
                key_spec,
                value_spec,
            } => {
                out.write(&[MAP])?
                    + size.encode(out)?
                    + Spec::to_bytes_internal(key_spec, out)?
                    + Spec::to_bytes_internal(value_spec, out)?
            }
            Spec::List { value_spec, size } => {
                out.write(&[LIST])? + size.encode(out)? + Spec::to_bytes_internal(value_spec, out)?
            }
            Spec::String(size, str_fmt) => {
                if matches!(size, Size::Variable) && matches!(str_fmt, StringEncodingFmt::Utf8) {
                    out.write(&[UTF8_STRING])?
                } else {
                    out.write(&[STRING])? + size.encode(out)? + str_fmt.encode(out)?
                }
            }
            Spec::Bytes(size) => out.write(&[BYTES])? + size.encode(out)?,
            Spec::Optional(optional_type) => {
                out.write(&[OPTIONAL])? + Spec::to_bytes_internal(&optional_type, out)?
            }
            Spec::Name { name, spec } => {
                out.write(&[NAME])?
                    + encode_string_utf8(name, out)?
                    + Spec::to_bytes_internal(spec, out)?
            }
            Spec::Ref { name } => out.write(&[REF])? + encode_string_utf8(name, out)?,
            Spec::Record(fields) => {
                out.write(&[RECORD])?
                    + variable_length_encode_u64(fields.len() as u64, out)?
                    + fields
                        .iter()
                        .map(|(name, spec)| {
                            combine(
                                encode_string_utf8(name, out),
                                Spec::to_bytes_internal(&spec, out),
                            )
                        })
                        .fold(Ok(0usize), combine)?
            }
            Spec::Tuple(fields) => {
                out.write(&[TUPLE])?
                    + variable_length_encode_u64(fields.len() as u64, out)?
                    + fields
                        .iter()
                        .map(|spec| Spec::to_bytes_internal(&spec, out))
                        .fold(Ok(0usize), combine)?
            }
            Spec::Enum(variants) => {
                out.write(&[ENUM])?
                    + variable_length_encode_u64(variants.len() as u64, out)?
                    + variants
                        .iter()
                        .map(|(name, spec)| {
                            combine(
                                encode_string_utf8(name, out),
                                Spec::to_bytes_internal(spec, out),
                            )
                        })
                        .fold(Ok(0usize), combine)?
            }
            Spec::Union(variants) => {
                out.write(&[UNION])?
                    + variable_length_encode_u64(variants.len() as u64, out)?
                    + variants
                        .iter()
                        .map(|spec| Spec::to_bytes_internal(spec, out))
                        .fold(Ok(0usize), combine)?
            }
            Spec::Void => out.write(&[VOID])?,
        })
    }
}

#[inline]
fn encode_string_utf8<W: Write>(string: &String, out: &mut W) -> Result<usize, io::Error> {
    let b = string.as_bytes();
    Ok(variable_length_encode_u64(b.len() as u64, out)? + out.write(b)?)
}

fn decode_utf8_string<R: Read>(input: &mut R) -> Result<String, SpecParsingError> {
    let n = decode_u64(input)?;
    let mut s = String::with_capacity(n as usize);
    let n_actual = input.take(n).read_to_string(&mut s)?;
    if (n_actual as u64) < n {
        Err(SpecParsingError::UnexpectedEndOfBytes)
    } else {
        Ok(s)
    }
}

fn decode_u64<R: Read>(input: &mut R) -> Result<u64, SpecParsingError> {
    match variable_length_decode_u64(input)? {
        util::VariableLengthResult::Respresentable(n) => Ok(n),
        util::VariableLengthResult::Unrepresentable(v) => {
            return Err(SpecParsingError::IntegerOverflowVariableLengthDecodingError(v))
        }
    }
}

#[inline]
fn next_byte<R: Read>(input: &mut R) -> Result<u8, SpecParsingError> {
    let mut flag: u8 = 255;
    if 0usize == input.read(slice::from_mut(&mut flag))? {
        Err(SpecParsingError::UnexpectedEndOfBytes)
    } else {
        Ok(flag)
    }
}

#[derive(Debug)]
pub enum SpecParsingError {
    ReadError(io::Error),
    UnexpectedEndOfBytes,
    UnknownSpecFlag(u8),
    UnknownBinaryFormatFlag(u8),
    UnknownDecimalFormatFlag(u8),
    UnknownStringFormatFlag(u8),
    UnknownSizeFormatFlag(u8),
    VariableLengthDecodingError(util::VariableLengthDecodingError),
    IntegerOverflowVariableLengthDecodingError(Vec<u8>),
}

impl From<io::Error> for SpecParsingError {
    fn from(e: io::Error) -> Self {
        match e.kind() {
            io::ErrorKind::UnexpectedEof => SpecParsingError::UnexpectedEndOfBytes,
            _ => SpecParsingError::ReadError(e),
        }
    }
}

impl From<util::VariableLengthDecodingError> for SpecParsingError {
    fn from(e: util::VariableLengthDecodingError) -> Self {
        match e {
            VariableLengthDecodingError::IncompleteVariableLengthEncoding => {
                SpecParsingError::UnexpectedEndOfBytes
            }
            _ => SpecParsingError::VariableLengthDecodingError(e),
        }
    }
}

#[derive(Debug, Hash, Eq, PartialEq, Clone)]
pub enum Size {
    Fixed(u64),
    Variable,
}

impl Size {
    #[inline]
    pub(crate) fn encode<W: Write>(&self, out: &mut W) -> Result<usize, io::Error> {
        match self {
            Size::Fixed(n) => combine(out.write(&[0]), variable_length_encode_u64(*n, out)),
            Size::Variable => out.write(&[1]),
        }
    }

    #[inline]
    pub(crate) fn decode<R: Read>(input: &mut R) -> Result<Size, SpecParsingError> {
        match next_byte(input)? {
            0 => Ok(Size::Fixed(decode_u64(input)?)),
            1 => Ok(Size::Variable),
            b => Err(SpecParsingError::UnknownSizeFormatFlag(b)),
        }
    }
}

#[derive(Debug, Hash, Eq, PartialEq, Clone, EnumIter)]
pub enum InterchangeBinaryFloatingPointFormat {
    Half,
    Single,
    Double,
    Quadruple,
    Octuple,
}

impl InterchangeBinaryFloatingPointFormat {
    fn significand_bits(&self) -> u64 {
        match self {
            InterchangeBinaryFloatingPointFormat::Half => 11,
            InterchangeBinaryFloatingPointFormat::Single => 24,
            InterchangeBinaryFloatingPointFormat::Double => 53,
            InterchangeBinaryFloatingPointFormat::Quadruple => 113,
            InterchangeBinaryFloatingPointFormat::Octuple => 237,
        }
    }

    fn exponent_bits(&self) -> u64 {
        match self {
            InterchangeBinaryFloatingPointFormat::Half => 5,
            InterchangeBinaryFloatingPointFormat::Single => 8,
            InterchangeBinaryFloatingPointFormat::Double => 11,
            InterchangeBinaryFloatingPointFormat::Quadruple => 15,
            InterchangeBinaryFloatingPointFormat::Octuple => 19,
        }
    }

    #[inline]
    pub(crate) fn encode<W: Write>(&self, out: &mut W) -> Result<usize, io::Error> {
        match self {
            InterchangeBinaryFloatingPointFormat::Half => out.write(&[0]),
            InterchangeBinaryFloatingPointFormat::Single => out.write(&[1]),
            InterchangeBinaryFloatingPointFormat::Double => out.write(&[2]),
            InterchangeBinaryFloatingPointFormat::Quadruple => out.write(&[3]),
            InterchangeBinaryFloatingPointFormat::Octuple => out.write(&[4]),
        }
    }

    #[inline]
    pub(crate) fn decode<R: Read>(
        input: &mut R,
    ) -> Result<InterchangeBinaryFloatingPointFormat, SpecParsingError> {
        Ok(match next_byte(input)? {
            0 => InterchangeBinaryFloatingPointFormat::Half,
            1 => InterchangeBinaryFloatingPointFormat::Single,
            2 => InterchangeBinaryFloatingPointFormat::Double,
            3 => InterchangeBinaryFloatingPointFormat::Quadruple,
            4 => InterchangeBinaryFloatingPointFormat::Octuple,
            b => return Err(SpecParsingError::UnknownBinaryFormatFlag(b)),
        })
    }
}

#[derive(Debug, Hash, Eq, PartialEq, Clone, EnumIter)]
pub enum InterchangeDecimalFloatingPointFormat {
    Dec32,
    Dec64,
    Dec128,
}

impl InterchangeDecimalFloatingPointFormat {
    fn significand_bits(&self) -> u64 {
        match self {
            InterchangeDecimalFloatingPointFormat::Dec32 => 7,
            InterchangeDecimalFloatingPointFormat::Dec64 => 16,
            InterchangeDecimalFloatingPointFormat::Dec128 => 34,
        }
    }

    fn decimal_digits(&self) -> u64 {
        match self {
            InterchangeDecimalFloatingPointFormat::Dec32 => 7,
            InterchangeDecimalFloatingPointFormat::Dec64 => 16,
            InterchangeDecimalFloatingPointFormat::Dec128 => 34,
        }
    }

    #[inline]
    pub(crate) fn encode<W: Write>(&self, out: &mut W) -> Result<usize, io::Error> {
        match self {
            InterchangeDecimalFloatingPointFormat::Dec32 => out.write(&[0]),
            InterchangeDecimalFloatingPointFormat::Dec64 => out.write(&[1]),
            InterchangeDecimalFloatingPointFormat::Dec128 => out.write(&[2]),
        }
    }

    #[inline]
    pub(crate) fn decode<R: Read>(
        input: &mut R,
    ) -> Result<InterchangeDecimalFloatingPointFormat, SpecParsingError> {
        Ok(match next_byte(input)? {
            0 => InterchangeDecimalFloatingPointFormat::Dec32,
            1 => InterchangeDecimalFloatingPointFormat::Dec64,
            2 => InterchangeDecimalFloatingPointFormat::Dec128,
            b => return Err(SpecParsingError::UnknownDecimalFormatFlag(b)),
        })
    }
}

#[derive(Debug, Hash, Eq, PartialEq, Clone, Default, EnumIter)]
pub enum StringEncodingFmt {
    #[default]
    Utf8, //use this one, please
    Utf16,
    Ascii,
}

impl StringEncodingFmt {
    #[inline]
    pub(crate) fn encode<W: Write>(&self, out: &mut W) -> Result<usize, io::Error> {
        match self {
            StringEncodingFmt::Utf8 => out.write(&[0]),
            StringEncodingFmt::Utf16 => out.write(&[1]),
            StringEncodingFmt::Ascii => out.write(&[2]),
        }
    }

    #[inline]
    pub(crate) fn decode<R: Read>(input: &mut R) -> Result<StringEncodingFmt, SpecParsingError> {
        Ok(match next_byte(input)? {
            0 => StringEncodingFmt::Utf8,
            1 => StringEncodingFmt::Utf16,
            2 => StringEncodingFmt::Ascii,
            b => return Err(SpecParsingError::UnknownStringFormatFlag(b)),
        })
    }
}

fn combine(a: Result<usize, io::Error>, b: Result<usize, io::Error>) -> Result<usize, io::Error> {
    match (a.as_ref(), b.as_ref()) {
        (Ok(i), Ok(j)) => Ok(i + j),
        (Err(_), _) => a,
        (_, Err(_)) => b,
    }
}

#[cfg(test)]
mod tests {

    use crate::spec;

    use super::*;
    use std::{io::Cursor, iter};
    use strum::IntoEnumIterator;

    fn get_all_kinds_spec() -> Vec<Spec> {
        let mut specs = Vec::with_capacity(256);
        for spec_kind in SpecKind::iter() {
            for spec in get_valid_specs_for_kind(spec_kind) {
                specs.push(spec);
            }
        }
        assert!(!specs.is_empty());
        specs
    }

    fn get_valid_specs_for_kind(spec_kind: SpecKind) -> Box<dyn Iterator<Item = Spec>> {
        match spec_kind {
            SpecKind::Bool => Box::new(iter::once(Spec::Bool)),
            SpecKind::Uint => Box::new((0..=u8::MAX).map(|n| Spec::Uint(n)).into_iter()),
            SpecKind::Int => Box::new((0..=u8::MAX).map(|n| Spec::Int(n)).into_iter()),
            SpecKind::BinaryFloatingPoint => Box::new(
                InterchangeBinaryFloatingPointFormat::iter()
                    .map(|bfp| Spec::BinaryFloatingPoint(bfp)),
            ),
            SpecKind::DecimalFloatingPoint => Box::new(
                InterchangeDecimalFloatingPointFormat::iter()
                    .map(|dfp| Spec::DecimalFloatingPoint(dfp)),
            ),
            SpecKind::Decimal => Box::new(
                vec![
                    Spec::Decimal {
                        precision: 22,
                        scale: 2,
                    },
                    Spec::Decimal {
                        precision: 10,
                        scale: 2,
                    },
                    Spec::Decimal {
                        precision: 77,
                        scale: 10,
                    },
                    Spec::Decimal {
                        precision: 40,
                        scale: 10,
                    },
                ]
                .into_iter(),
            ),
            SpecKind::Map => Box::new(
                vec![
                    Spec::Map {
                        size: Size::Variable,
                        key_spec: Spec::Bool.into(),
                        value_spec: Spec::Int(4).into(),
                    },
                    Spec::Map {
                        size: Size::Fixed(50),
                        key_spec: Spec::Int(4).into(),
                        value_spec: Spec::Int(4).into(),
                    },
                ]
                .into_iter(),
            ),
            SpecKind::List => Box::new(
                vec![
                    Spec::List {
                        size: Size::Variable,
                        value_spec: Spec::BinaryFloatingPoint(
                            InterchangeBinaryFloatingPointFormat::Double,
                        )
                        .into(),
                    },
                    Spec::List {
                        size: Size::Fixed(32),
                        value_spec: Spec::Decimal {
                            precision: 10,
                            scale: 2,
                        }
                        .into(),
                    },
                ]
                .into_iter(),
            ),
            SpecKind::String => Box::new(
                iter::once(Spec::String(Size::Variable, StringEncodingFmt::Utf8))
                    .chain(StringEncodingFmt::iter().map(|fmt| Spec::String(Size::Fixed(45), fmt)))
                    .chain(StringEncodingFmt::iter().map(|fmt| Spec::String(Size::Variable, fmt))),
            ),
            SpecKind::Bytes => Box::new(
                iter::once(Spec::Bytes(Size::Variable))
                    .chain(iter::once(Spec::Bytes(Size::Fixed(1024)))),
            ),
            SpecKind::Optional => Box::new(
                iter::once(Spec::Optional(Spec::Bytes(Size::Variable).into()))
                    .chain(iter::once(Spec::Optional(Spec::Int(6).into()))),
            ),
            SpecKind::Name => Box::new(
                iter::once(Spec::Name {
                    name: "test".into(),
                    spec: Spec::List {
                        size: Size::Fixed(32),
                        value_spec: Spec::Decimal {
                            precision: 10,
                            scale: 2,
                        }
                        .into(),
                    }
                    .into(),
                })
                .chain(iter::once(Spec::Name {
                    name: "test".into(),
                    spec: Spec::Bytes(Size::Variable).into(),
                })),
            ),
            SpecKind::Ref => Box::new(iter::once(Spec::Name {
                name: "test".into(),
                spec: Box::new(Spec::Record(vec![
                    ("field1".into(), Spec::Bool),
                    ("field2".into(), Spec::Int(4)),
                    (
                        "field3".into(),
                        Spec::Optional(Box::new(Spec::Ref {
                            name: "test".into(),
                        })),
                    ),
                ])),
            })),
            SpecKind::Record => Box::new(iter::once(Spec::Record(vec![
                ("field1".into(), Spec::Bool),
                ("field2".into(), Spec::Int(4)),
            ]))),
            SpecKind::Tuple => Box::new(iter::once(Spec::Tuple(vec![Spec::Bool, Spec::Int(4)]))),
            SpecKind::Enum => Box::new(iter::once(Spec::Enum(vec![
                ("field1".into(), Spec::Bool),
                ("field2".into(), Spec::Int(4)),
            ]))),
            SpecKind::Union => Box::new(iter::once(Spec::Union(vec![Spec::Bool, Spec::Int(4)]))),
            SpecKind::Void => Box::new(iter::once(Spec::Void)),
        }
    }

    #[test]
    fn test_serde() {
        fn test_spec_serde(spec: Spec) {
            assert_eq!(
                spec,
                Spec::read_from_bytes(&mut Cursor::new(spec.to_bytes()))
                    .expect(format!("Unable to read {:?}", spec).as_str())
            );
        }
        for spec in get_all_kinds_spec() {
            test_spec_serde(spec);
        }
    }

    #[test]
    fn test_longform_serde() {
        fn test_spec_longform_serde(spec: Spec) {
            assert_eq!(
                spec,
                Spec::read_from_bytes(&mut Cursor::new(spec.to_longform_bytes()))
                    .expect(format!("Unable to read {:?}", spec).as_str())
            );
        }
        for spec in get_all_kinds_spec() {
            test_spec_longform_serde(spec);
        }
    }

    #[test]
    fn test_write_size() {
        fn test_spec_write_size(spec: Spec) {
            let mut v = Vec::new();
            let reported_size = spec
                .write_as_bytes(&mut v)
                .expect(format!("Unable to write to bytes. Spec: {}", stringify!($spec)).as_str());
            assert_eq!(v.len(), reported_size);
        }
        for spec in get_all_kinds_spec() {
            test_spec_write_size(spec);
        }
    }

    #[test]
    fn test_EOF_deserialization() {
        fn test_eof_exception(spec: Spec) {
            let mut v = Vec::new();
            spec.write_as_bytes(&mut v)
                .expect(format!("Unable to write to bytes. Spec: {:?}", spec).as_str());
            v.truncate(v.len() / 2);
            let res: Result<Spec, SpecParsingError> = Spec::read_from_bytes(&mut Cursor::new(&v));
            if let SpecParsingError::UnexpectedEndOfBytes =
                res.expect_err("Unexpectedly parsed bytes to Spec")
            {
                assert!(true);
            } else {
                assert!(
                    false,
                    "EOF error expected for spec: {:?} with bytes {:?}",
                    spec, v
                );
            }
        }

        for spec in get_all_kinds_spec() {
            test_eof_exception(spec);
        }
    }
}
