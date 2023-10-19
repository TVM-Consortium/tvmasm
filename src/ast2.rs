use std::str::FromStr;

use chumsky::error::Error;
use chumsky::prelude::*;
use chumsky::util::MaybeRef;
use everscale_types::prelude::{Cell, CellBuilder};
use num_bigint::BigInt;
use num_traits::Num;

pub type Span = SimpleSpan<usize>;

pub fn parse(s: &'_ str) -> ParseResult<Vec<Instr<'_>>, ParserError> {
    parser().parse(s)
}

#[derive(Debug, Clone)]
pub struct Instr<'a> {
    pub span: Span,
    pub ident: &'a str,
    pub args: Vec<InstrArg<'a>>,
}

#[derive(Debug, Clone)]
pub struct InstrArg<'a> {
    pub span: Span,
    pub value: InstrArgValue<'a>,
}

#[derive(Debug, Clone)]
pub enum InstrArgValue<'a> {
    Nat(BigInt),
    SReg(i16),
    CReg(u8),
    Slice(Cell),
    Block(Vec<Instr<'a>>),
    Invalid,
}

fn parser<'a>() -> impl Parser<'a, &'a str, Vec<Instr<'a>>, extra::Err<ParserError>> {
    instr().padded().repeated().collect()
}

fn instr<'a>() -> impl Parser<'a, &'a str, Instr<'a>, extra::Err<ParserError>> {
    recursive(|instr| {
        let instr_arg = choice((
            nat().map(InstrArgValue::Nat),
            stack_register().map(|idx| {
                idx.map(InstrArgValue::SReg)
                    .unwrap_or(InstrArgValue::Invalid)
            }),
            control_register().map(|idx| {
                idx.map(InstrArgValue::CReg)
                    .unwrap_or(InstrArgValue::Invalid)
            }),
            cont_block(instr).map(InstrArgValue::Block),
            cell_slice().map(|slice| {
                slice
                    .map(InstrArgValue::Slice)
                    .unwrap_or(InstrArgValue::Invalid)
            }),
        ))
        .map_with(|value, e| InstrArg {
            value,
            span: e.span(),
        });

        let args = instr_arg
            .separated_by(just(',').padded().recover_with(skip_then_retry_until(
                any().ignored(),
                choice((just(',').ignored(), text::newline())),
            )))
            .collect::<Vec<_>>();

        instr_ident()
            .padded()
            .then(args)
            .map_with(|(ident, args), e| Instr {
                ident,
                span: e.span(),
                args,
            })
    })
}

fn instr_ident<'a>() -> impl Parser<'a, &'a str, &'a str, extra::Err<ParserError>> + Clone {
    fn is_instr_ident_char(c: char, ext: bool) -> bool {
        c.is_ascii_uppercase() || c.is_ascii_digit() || c == '#' || c == '_' || ext && c == ':'
    }

    any()
        .try_map(|c, span: Span| {
            if is_instr_ident_char(c, false) {
                Ok(c)
            } else {
                Err(ParserError::expected_found(
                    [],
                    Some(MaybeRef::Val(c)),
                    span,
                ))
            }
        })
        .then(any().filter(|c| is_instr_ident_char(*c, true)).repeated())
        .to_slice()
}

fn nat<'a>() -> impl Parser<'a, &'a str, BigInt, extra::Err<ParserError>> + Clone {
    fn parse_int(s: &str, radix: u32, span: Span) -> Result<BigInt, ParserError> {
        match BigInt::from_str_radix(s, radix) {
            Ok(n) => Ok(n),
            Err(e) => Err(ParserError::InvalidInt { span, inner: e }),
        }
    }

    let number = choice((
        just("0x")
            .ignore_then(text::int(16))
            .try_map(|s, span| parse_int(s, 16, span)),
        just("0b")
            .ignore_then(text::int(2))
            .try_map(|s, span| parse_int(s, 2, span)),
        text::int(10).try_map(|s, span| parse_int(s, 10, span)),
    ));

    choice((
        just('-').ignore_then(number).map(std::ops::Neg::neg),
        number,
    ))
}

fn stack_register<'a>() -> impl Parser<'a, &'a str, Option<i16>, extra::Err<ParserError>> + Clone {
    let until_next_arg = any()
        .filter(|&c: &char| c != ',' && !c.is_whitespace())
        .repeated();

    let until_eof_or_paren = none_of(")\n").repeated().then(just(')').or_not());

    let idx =
        text::int::<_, _, extra::Err<ParserError>>(10).try_map(|s, span| match i16::from_str(s) {
            Ok(n) => Ok(n),
            Err(e) => Err(ParserError::InvalidStackRegister {
                span,
                inner: e.into(),
            }),
        });

    just('s').ignore_then(
        choice((
            just('(').ignore_then(
                just('-')
                    .ignore_then(idx)
                    .map(|idx| Some(-idx))
                    .then_ignore(just(')'))
                    .recover_with(via_parser(until_eof_or_paren.map(|_| None))),
            ),
            idx.map(Some),
        ))
        .recover_with(via_parser(until_next_arg.map(|_| None))),
    )
}

fn control_register<'a>() -> impl Parser<'a, &'a str, Option<u8>, extra::Err<ParserError>> + Clone {
    let recovery = any()
        .filter(|&c: &char| c != ',' && !c.is_whitespace())
        .repeated();

    let idx = text::int::<_, _, extra::Err<ParserError>>(10)
        .try_map(|s, span| match u8::from_str(s) {
            Ok(n) if (0..=5).contains(&n) || n == 7 => Ok(Some(n)),
            Ok(n) => Err(ParserError::InvalidControlRegister {
                span,
                inner: ControlRegisterError::OutOfRange(n).into(),
            }),
            Err(e) => Err(ParserError::InvalidControlRegister {
                span,
                inner: e.into(),
            }),
        })
        .recover_with(via_parser(recovery.map(|_| None)));

    just('c').ignore_then(idx)
}

fn cont_block<'a>(
    instr: Recursive<dyn Parser<'a, &'a str, Instr<'a>, extra::Err<ParserError>> + 'a>,
) -> impl Parser<'a, &'a str, Vec<Instr<'a>>, extra::Err<ParserError>> + Clone {
    instr.padded().repeated().collect().delimited_by(
        just('{'),
        just('}')
            .ignored()
            .recover_with(via_parser(end()))
            .recover_with(skip_then_retry_until(any().ignored(), end())),
    )
}

fn cell_slice<'a>() -> impl Parser<'a, &'a str, Option<Cell>, extra::Err<ParserError>> + Clone {
    let content_recovery = any()
        .filter(|&c: &char| c != '}' && !c.is_whitespace())
        .repeated();

    let braces_recovery = none_of("}\n").repeated().then(just('}').or_not());

    let make_slice_parser =
        |prefix: &'static str, parser: fn(&'a str) -> Result<Cell, SliceError>| {
            just(prefix)
                .ignore_then(
                    any()
                        .filter(|&c: &char| c != '}' && !c.is_whitespace())
                        .repeated()
                        .to_slice()
                        .try_map(move |s, span| match (parser)(s) {
                            Ok(s) => Ok(Some(s)),
                            Err(e) => Err(ParserError::InvalidSlice {
                                span,
                                inner: e.into(),
                            }),
                        })
                        .recover_with(via_parser(content_recovery.map(|_| None))),
                )
                .then(
                    just('}')
                        .map(|_| true)
                        .recover_with(via_parser(braces_recovery.map(|_| false))),
                )
                .map(|(mut t, valid)| {
                    if !valid {
                        t = None;
                    }
                    t
                })
        };

    choice((
        make_slice_parser("x{", parse_hex_slice),
        make_slice_parser("b{", parse_bin_slice),
    ))
}

fn parse_hex_slice(s: &str) -> Result<Cell, SliceError> {
    fn hex_char(c: u8) -> Result<u8, SliceError> {
        match c {
            b'A'..=b'F' => Ok(c - b'A' + 10),
            b'a'..=b'f' => Ok(c - b'a' + 10),
            b'0'..=b'9' => Ok(c - b'0'),
            _ => Err(SliceError::InvalidHex(c as char)),
        }
    }

    if !s.is_ascii() {
        return Err(SliceError::NonAscii);
    }

    let s = s.as_bytes();
    let (mut s, with_tag) = match s.strip_suffix(b"_") {
        Some(s) => (s, true),
        None => (s, false),
    };

    let mut half_byte = None;
    if s.len() % 2 != 0 {
        if let Some((last, prefix)) = s.split_last() {
            half_byte = Some(hex_char(*last)?);
            s = prefix;
        }
    }

    if s.len() > 128 * 2 {
        return Err(SliceError::TooLong);
    }

    let mut builder = CellBuilder::new();

    let mut bytes = hex::decode(s)?;

    let mut bits = bytes.len() as u16 * 8;
    if let Some(half_byte) = half_byte {
        bits += 4;
        bytes.push(half_byte << 4);
    }

    if with_tag {
        bits = bytes.len() as u16 * 8;
        for byte in bytes.iter().rev() {
            if *byte == 0 {
                bits -= 8;
            } else {
                bits -= 1 + byte.trailing_zeros() as u16;
                break;
            }
        }
    }

    builder.store_raw(&bytes, bits)?;
    builder.build().map_err(SliceError::CellError)
}

fn parse_bin_slice(s: &str) -> Result<Cell, SliceError> {
    use everscale_types::cell::MAX_BIT_LEN;

    let mut bits = 0;
    let mut bytes = [0; 128];

    for char in s.chars() {
        let value = match char {
            '0' => 0u8,
            '1' => 1,
            c => return Err(SliceError::InvalidBin(c)),
        };
        bytes[bits / 8] |= value << (7 - bits % 8);

        bits += 1;
        if bits > MAX_BIT_LEN as usize {
            return Err(SliceError::TooLong);
        }
    }

    let mut builder = CellBuilder::new();
    builder.store_raw(&bytes, bits as _)?;
    builder.build().map_err(SliceError::CellError)
}

#[derive(Debug)]
pub enum ParserError {
    ExpectedFound {
        span: Span,
        expected: Vec<Option<char>>,
        found: Option<char>,
    },
    InvalidInt {
        span: Span,
        inner: num_bigint::ParseBigIntError,
    },
    InvalidStackRegister {
        span: Span,
        inner: Box<dyn std::error::Error>,
    },
    InvalidControlRegister {
        span: Span,
        inner: Box<dyn std::error::Error>,
    },
    InvalidSlice {
        span: Span,
        inner: Box<dyn std::error::Error>,
    },
}

#[derive(thiserror::Error, Debug)]
enum ControlRegisterError {
    #[error("control register `c{0}` is out of range")]
    OutOfRange(u8),
}

#[derive(thiserror::Error, Debug)]
enum SliceError {
    #[error("non-ascii characters in bitstring")]
    NonAscii,
    #[error("unexpected char `{0}` in hex bitstring")]
    InvalidHex(char),
    #[error("invalid hex bitstring: {0}")]
    InvalidHexFull(#[from] hex::FromHexError),
    #[error("unexpected char `{0}` in binary bitstring")]
    InvalidBin(char),
    #[error("bitstring is too long")]
    TooLong,
    #[error("cell build error: {0}")]
    CellError(#[from] everscale_types::error::Error),
}

impl<'a> chumsky::error::Error<'a, &'a str> for ParserError {
    fn expected_found<Iter: IntoIterator<Item = Option<MaybeRef<'a, char>>>>(
        expected: Iter,
        found: Option<MaybeRef<'a, char>>,
        span: Span,
    ) -> Self {
        Self::ExpectedFound {
            span,
            expected: expected
                .into_iter()
                .map(|e| e.as_deref().copied())
                .collect(),
            found: found.as_deref().copied(),
        }
    }

    fn merge(mut self, mut other: Self) -> Self {
        if let (
            Self::ExpectedFound { expected, .. },
            Self::ExpectedFound {
                expected: expected_other,
                ..
            },
        ) = (&mut self, &mut other)
        {
            expected.append(expected_other);
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_asm() {
        assert!(parse("").unwrap().is_empty());
    }

    #[test]
    fn simple_asm() {
        const CODE: &str = r#"
        PUSHCONT {
            PUSHREF x{afff_}
            PUSH s(-1)
            OVER
            LESSINT 2
            PUSHCONT {
                2DROP
                PUSHINT 1
            }
            IFJMP
            OVER
            DEC
            SWAP
            DUP
            EXECUTE
            MUL
        }
        DUP
        JMPX
        "#;

        let (output, errors) = parse(CODE).into_output_errors();
        println!("OUTPUT: {:#?}", output);
        println!("ERRORS: {:#?}", errors);
    }
}