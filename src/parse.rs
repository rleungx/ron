use std::char::from_u32 as char_from_u32;
use std::fmt::{Display, Formatter, Result as FmtResult};
use std::ops::Neg;
use std::result::Result as StdResult;
use std::str::{FromStr, from_utf8, from_utf8_unchecked};

use de::{Error, ParseError, Result};

const DIGITS: &[u8] = b"0123456789ABCDEFabcdef";
const FLOAT_CHARS: &[u8] = b"0123456789.+-eE";
const IDENT_FIRST: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz_";
const IDENT_CHAR: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz_0123456789";
const WHITE_SPACE: &[u8] = b"\n\t\r ";

#[derive(Clone, Copy, Debug)]
pub struct Bytes<'a> {
    /// Bits set according to `Extension` enum.
    pub exts: Extensions,
    bytes: &'a [u8],
    column: usize,
    line: usize,
}

impl<'a> Bytes<'a> {
    pub fn new(bytes: &'a [u8]) -> Result<Self> {
        let mut b = Bytes {
            bytes,
            column: 1,
            exts: Extensions::empty(),
            line: 1,
        };

        b.skip_ws();
        // Loop over all extensions attributes
        loop {
            let attribute = b.extensions()?;

            if attribute.is_empty() {
                break;
            }

            b.exts |= attribute;
            b.skip_ws();
        }

        Ok(b)
    }

    pub fn advance(&mut self, bytes: usize) -> Result<()> {
        for _ in 0..bytes {
            self.advance_single()?;
        }

        Ok(())
    }

    pub fn advance_single(&mut self) -> Result<()> {
        if self.peek_or_eof()? == b'\n' {
            self.line += 1;
            self.column = 1;
        } else {
            self.column += 1;
        }

        self.bytes = &self.bytes[1..];

        Ok(())
    }

    pub fn bool(&mut self) -> Result<bool> {
        if self.consume("true") {
            Ok(true)
        } else if self.consume("false") {
            Ok(false)
        } else {
            self.err(ParseError::ExpectedBoolean)
        }
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn char(&mut self) -> Result<char> {
        use std::cmp::min;

        if !self.consume("'") {
            return self.err(ParseError::ExpectedChar);
        }

        let c = self.peek_or_eof()?;

        let c = if c == b'\\' {
            let _ = self.advance(1);

            self.parse_escape()?
        } else {
            // Check where the end of the char (') is and try to
            // interpret the rest as UTF-8

            let max = min(5, self.bytes.len());
            let pos: usize = self.bytes[..max]
                .iter()
                .position(|&x| x == b'\'')
                .ok_or_else(|| self.error(ParseError::ExpectedChar))?;
            let s = from_utf8(&self.bytes[0..pos]).map_err(|e| self.error(e.into()))?;
            let mut chars = s.chars();

            let first = chars
                .next()
                .ok_or_else(|| self.error(ParseError::ExpectedChar))?;
            if chars.next().is_some() {
                return self.err(ParseError::ExpectedChar);
            }

            let _ = self.advance(pos);

            first
        };

        if !self.consume("'") {
            return self.err(ParseError::ExpectedChar);
        }

        Ok(c)
    }

    pub fn comma(&mut self) -> bool {
        self.skip_ws();

        if self.consume(",") {
            self.skip_ws();

            true
        } else {
            false
        }
    }

    /// Only returns true if the char after `ident` cannot belong
    /// to an identifier.
    pub fn check_ident(&mut self, ident: &str) -> bool {
        self.test_for(ident) && !self.check_ident_char(ident.len())
    }

    fn check_ident_char(&self, index: usize) -> bool {
        self.bytes
            .get(index)
            .map(|b| IDENT_CHAR.contains(b))
            .unwrap_or(false)
    }

    /// Only returns true if the char after `ident` cannot belong
    /// to an identifier.
    pub fn consume_ident(&mut self, ident: &str) -> bool {
        if self.check_ident(ident) {
            let _ = self.advance(ident.len());

            true
        } else {
            false
        }
    }

    pub fn consume(&mut self, s: &str) -> bool {
        if self.test_for(s) {
            let _ = self.advance(s.len());

            true
        } else {
            false
        }
    }

    fn consume_all(&mut self, all: &[&str]) -> bool {
        all.iter()
            .map(|elem| {
                if self.consume(elem) {
                    self.skip_ws();

                    true
                } else {
                    false
                }
            })
            .all(|b| b)
    }

    pub fn eat_byte(&mut self) -> Result<u8> {
        let peek = self.peek_or_eof()?;
        let _ = self.advance_single();

        Ok(peek)
    }

    pub fn err<T>(&self, kind: ParseError) -> Result<T> {
        Err(self.error(kind))
    }

    pub fn error(&self, kind: ParseError) -> Error {
        Error::Parser(
            kind,
            Position {
                line: self.line,
                col: self.column,
            },
        )
    }

    pub fn expect_byte(&mut self, byte: u8, error: ParseError) -> Result<()> {
        self.eat_byte().and_then(|b| match b == byte {
            true => Ok(()),
            false => self.err(error),
        })
    }

    /// Returns the extensions bit mask.
    fn extensions(&mut self) -> Result<Extensions> {
        if self.peek() != Some(b'#') {
            return Ok(Extensions::empty());
        }

        if !self.consume_all(&["#", "!", "[", "enable", "("]) {
            return self.err(ParseError::ExpectedAttribute);
        }

        self.skip_ws();
        let mut extensions = Extensions::empty();

        loop {
            let ident = self.identifier()?;
            let extension = Extensions::from_ident(ident).ok_or_else(|| {
                self.error(ParseError::NoSuchExtension(
                    from_utf8(ident).unwrap().to_owned(),
                ))
            })?;

            extensions |= extension;

            let comma = self.comma();

            // If we have no comma but another item, return an error
            if !comma && self.check_ident_char(0) {
                return self.err(ParseError::ExpectedComma);
            }

            // If there's no comma, assume the list ended.
            // If there is, it might be a trailing one, thus we only
            // continue the loop if we get an ident char.
            if !comma || !self.check_ident_char(0) {
                break;
            }
        }

        self.skip_ws();

        match self.consume_all(&[")", "]"]) {
            true => Ok(extensions),
            false => Err(self.error(ParseError::ExpectedAttributeEnd)),
        }
    }

    pub fn float<T>(&mut self) -> Result<T>
    where
        T: FromStr,
    {
        let num_bytes = self.next_bytes_contained_in(FLOAT_CHARS);

        let s = unsafe { from_utf8_unchecked(&self.bytes[0..num_bytes]) };
        let res = FromStr::from_str(s).map_err(|_| self.error(ParseError::ExpectedFloat));

        let _ = self.advance(num_bytes);

        res
    }

    pub fn identifier(&mut self) -> Result<&'a [u8]> {
        if IDENT_FIRST.contains(&self.peek_or_eof()?) {
            let bytes = self.next_bytes_contained_in(IDENT_CHAR);

            let ident = &self.bytes[..bytes];
            let _ = self.advance(bytes);

            Ok(ident)
        } else {
            self.err(ParseError::ExpectedIdentifier)
        }
    }

    pub fn next_bytes_contained_in(&self, allowed: &[u8]) -> usize {
        self.bytes
            .iter()
            .take_while(|b| allowed.contains(b))
            .fold(0, |acc, _| acc + 1)
    }

    pub fn skip_ws(&mut self) {
        while self.peek()
            .map(|c| WHITE_SPACE.contains(&c))
            .unwrap_or(false)
        {
            let _ = self.advance_single();
        }

        if self.skip_comment() {
            self.skip_ws();
        }
    }

    pub fn peek(&self) -> Option<u8> {
        self.bytes.get(0).map(|b| *b)
    }

    pub fn peek_or_eof(&self) -> Result<u8> {
        self.bytes
            .get(0)
            .map(|b| *b)
            .ok_or(self.error(ParseError::Eof))
    }

    pub fn signed_integer<T>(&mut self) -> Result<T>
    where
        T: Neg<Output = T> + Num,
    {
        match self.peek_or_eof()? {
            b'+' => {
                let _ = self.advance_single();

                self.unsigned_integer()
            }
            b'-' => {
                let _ = self.advance_single();

                self.unsigned_integer::<T>().map(Neg::neg)
            }
            _ => self.unsigned_integer(),
        }
    }

    pub fn string(&mut self) -> Result<ParsedStr> {
        use std::iter::repeat;

        if !self.consume("\"") {
            return self.err(ParseError::ExpectedString);
        }

        let (i, end_or_escape) = self.bytes
            .iter()
            .enumerate()
            .find(|&(_, &b)| b == b'\\' || b == b'"')
            .ok_or(self.error(ParseError::ExpectedStringEnd))?;

        if *end_or_escape == b'"' {
            let s = from_utf8(&self.bytes[..i]).map_err(|e| self.error(e.into()))?;

            // Advance by the number of bytes of the string
            // + 1 for the `"`.
            let _ = self.advance(i + 1);

            Ok(ParsedStr::Slice(s))
        } else {
            let mut i = i;
            let mut s: Vec<_> = self.bytes[..i].to_vec();

            loop {
                let _ = self.advance(i + 1);
                let character = self.parse_escape()?;
                match character.len_utf8() {
                    1 => s.push(character as u8),
                    len => {
                        let start = s.len();
                        s.extend(repeat(0).take(len));
                        character.encode_utf8(&mut s[start..]);
                    }
                }

                let (new_i, end_or_escape) = self.bytes
                    .iter()
                    .enumerate()
                    .find(|&(_, &b)| b == b'\\' || b == b'"')
                    .ok_or(ParseError::Eof)
                    .map_err(|e| self.error(e))?;

                i = new_i;
                s.extend_from_slice(&self.bytes[..i]);

                if *end_or_escape == b'"' {
                    let _ = self.advance(i + 1);

                    let s = String::from_utf8(s).map_err(|e| self.error(e.into()))?;
                    break Ok(ParsedStr::Allocated(s));
                }
            }
        }
    }

    fn test_for(&self, s: &str) -> bool {
        s.bytes()
            .enumerate()
            .all(|(i, b)| self.bytes.get(i).map(|t| *t == b).unwrap_or(false))
    }

    pub fn unsigned_integer<T: Num>(&mut self) -> Result<T> {
        let base = if self.peek() == Some(b'0') {
            match self.bytes.get(1).cloned() {
                Some(b'x') => 16,
                Some(b'b') => 2,
                Some(b'o') => 8,
                _ => 10,
            }
        } else {
            10
        };

        if base != 10 {
            // If we have `0x45A` for example,
            // cut it to `45A`.
            let _ = self.advance(2);
        }

        let num_bytes = self.next_bytes_contained_in(DIGITS);

        if num_bytes == 0 {
            return self.err(ParseError::ExpectedInteger);
        }

        let res = Num::from_str(
            unsafe { from_utf8_unchecked(&self.bytes[0..num_bytes]) },
            base,
        ).map_err(|_| self.error(ParseError::ExpectedInteger));

        let _ = self.advance(num_bytes);

        res
    }

    fn decode_ascii_escape(&mut self) -> Result<u8> {
        let mut n = 0;
        for _ in 0..2 {
            n = n << 4;
            let byte = self.eat_byte()?;
            let decoded = self.decode_hex(byte)?;
            n |= decoded;
        }

        Ok(n)
    }

    fn decode_hex(&self, c: u8) -> Result<u8> {
        match c {
            c @ b'0'...b'9' => Ok(c - b'0'),
            c @ b'a'...b'f' => Ok(10 + c - b'a'),
            c @ b'A'...b'F' => Ok(10 + c - b'A'),
            _ => self.err(ParseError::InvalidEscape("Non-hex digit found")),
        }
    }

    fn parse_escape(&mut self) -> Result<char> {
        let c = match self.eat_byte()? {
            b'\'' => '\'',
            b'"' => '"',
            b'\\' => '\\',
            b'n' => '\n',
            b'r' => '\r',
            b't' => '\t',
            b'x' => self.decode_ascii_escape()? as char,
            b'u' => {
                self.expect_byte(b'{', ParseError::InvalidEscape("Missing {"))?;

                let mut bytes: u32 = 0;
                let mut num_digits = 0;

                while num_digits < 6 {
                    let byte = self.peek_or_eof()?;

                    if byte == b'}' {
                        break;
                    } else {
                        self.advance_single()?;
                    }

                    let byte = self.decode_hex(byte)?;
                    bytes = bytes << 4;
                    bytes |= byte as u32;

                    num_digits += 1;
                }

                if num_digits == 0 {
                    return self.err(ParseError::InvalidEscape(
                        "Expected 1-6 digits, got 0 digits",
                    ));
                }

                self.expect_byte(b'}', ParseError::InvalidEscape("No } at the end"))?;
                let character = char_from_u32(bytes)
                    .ok_or_else(|| self.error(ParseError::InvalidEscape("Not a valid char")))?;
                character
            }
            _ => {
                return self.err(ParseError::InvalidEscape("Unknown escape character"));
            }
        };

        Ok(c)
    }

    fn skip_comment(&mut self) -> bool {
        if self.consume("//") {
            let bytes = self.bytes.iter().take_while(|&&b| b != b'\n').count();

            let _ = self.advance(bytes);

            true
        } else {
            false
        }
    }
}

bitflags! {
    pub struct Extensions: usize {
        const UNWRAP_NEWTYPES = 0x1;
        const IMPLICIT_SOME = 0x2;
    }
}

impl Extensions {
    /// Creates an extension flag from an ident.
    pub fn from_ident(ident: &[u8]) -> Option<Extensions> {
        match ident {
            b"unwrap_newtypes" => Some(Extensions::UNWRAP_NEWTYPES),
            b"implicit_some" => Some(Extensions::IMPLICIT_SOME),
            _ => None,
        }
    }
}

pub trait Num: Sized {
    fn from_str(src: &str, radix: u32) -> StdResult<Self, ()>;
}

macro_rules! impl_num {
    ($ty:ident) => {
        impl Num for $ty {
            fn from_str(src: &str, radix: u32) -> StdResult<Self, ()> {
                $ty::from_str_radix(src, radix).map_err(|_| ())
            }
        }
    };
    ($($tys:ident)*) => {
        $( impl_num!($tys); )*
    };
}

impl_num!(u8 u16 u32 u64 i8 i16 i32 i64);

#[derive(Clone, Debug)]
pub enum ParsedStr<'a> {
    Allocated(String),
    Slice(&'a str),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Position {
    pub col: usize,
    pub line: usize,
}

impl Display for Position {
    fn fmt(&self, f: &mut Formatter) -> FmtResult {
        write!(f, "{}:{}", self.line, self.col)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_x10() {
        let mut bytes = Bytes::new(b"10").unwrap();
        assert_eq!(bytes.decode_ascii_escape(), Ok(0x10));
    }
}
