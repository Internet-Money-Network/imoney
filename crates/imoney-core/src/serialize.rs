//! Canonical binary encoding used for hashing and storage.
//!
//! Integers are fixed-width big-endian. Byte strings and lists carry a `u32` length prefix.
//! Every value has exactly one valid encoding, and decoding rejects trailing bytes.

use crate::hash::Hash;
use thiserror::Error;

#[derive(Error, Debug, PartialEq, Eq)]
pub enum DecodeError {
    #[error("Unexpected end of input")]
    UnexpectedEof,
    #[error("Length {0} exceeds limit {1}")]
    LengthExceedsLimit(usize, usize),
    #[error("{0} trailing bytes after value")]
    TrailingBytes(usize),
    #[error("Invalid value: {0}")]
    Invalid(&'static str),
}

/// A value with a canonical binary encoding.
pub trait Encode {
    fn encode(&self, out: &mut Vec<u8>);

    fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode(&mut out);
        out
    }
}

/// A value that can be read back from its canonical binary encoding.
pub trait Decode: Sized {
    fn decode(reader: &mut Reader<'_>) -> Result<Self, DecodeError>;

    /// Decodes a value that must span the whole input.
    fn from_bytes(bytes: &[u8]) -> Result<Self, DecodeError> {
        let mut reader = Reader::new(bytes);
        let value = Self::decode(&mut reader)?;
        reader.finish()?;
        Ok(value)
    }
}

/// Appends a length-prefixed byte string.
pub fn put_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    out.extend_from_slice(bytes);
}

/// Appends a length-prefixed list.
pub fn put_list<T: Encode>(out: &mut Vec<u8>, items: &[T]) {
    out.extend_from_slice(&(items.len() as u32).to_be_bytes());
    for item in items {
        item.encode(out);
    }
}

/// Bounds-checked cursor over an encoded buffer.
pub struct Reader<'a> {
    buf: &'a [u8],
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf }
    }

    pub fn remaining(&self) -> usize {
        self.buf.len()
    }

    pub fn take(&mut self, n: usize) -> Result<&'a [u8], DecodeError> {
        if self.buf.len() < n {
            return Err(DecodeError::UnexpectedEof);
        }
        let (head, tail) = self.buf.split_at(n);
        self.buf = tail;
        Ok(head)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], DecodeError> {
        Ok(self.take(N)?.try_into().unwrap())
    }

    pub fn u8(&mut self) -> Result<u8, DecodeError> {
        Ok(self.array::<1>()?[0])
    }

    pub fn u16(&mut self) -> Result<u16, DecodeError> {
        Ok(u16::from_be_bytes(self.array()?))
    }

    pub fn u32(&mut self) -> Result<u32, DecodeError> {
        Ok(u32::from_be_bytes(self.array()?))
    }

    pub fn u64(&mut self) -> Result<u64, DecodeError> {
        Ok(u64::from_be_bytes(self.array()?))
    }

    pub fn u128(&mut self) -> Result<u128, DecodeError> {
        Ok(u128::from_be_bytes(self.array()?))
    }

    pub fn hash(&mut self) -> Result<Hash, DecodeError> {
        Ok(Hash(self.array()?))
    }

    /// Reads a `u32` length prefix and checks it against `max`.
    pub fn len(&mut self, max: usize) -> Result<usize, DecodeError> {
        let len = self.u32()? as usize;
        if len > max {
            return Err(DecodeError::LengthExceedsLimit(len, max));
        }
        Ok(len)
    }

    /// Reads a length-prefixed byte string of at most `max` bytes.
    pub fn bytes(&mut self, max: usize) -> Result<Vec<u8>, DecodeError> {
        let len = self.len(max)?;
        Ok(self.take(len)?.to_vec())
    }

    /// Reads a length-prefixed list of at most `max` items.
    pub fn list<T: Decode>(&mut self, max: usize) -> Result<Vec<T>, DecodeError> {
        let len = self.len(max)?;
        // Every item occupies at least one byte, so a count beyond the remaining input is malformed
        if len > self.remaining() {
            return Err(DecodeError::UnexpectedEof);
        }
        let mut items = Vec::with_capacity(len);
        for _ in 0..len {
            items.push(T::decode(self)?);
        }
        Ok(items)
    }

    pub fn finish(&self) -> Result<(), DecodeError> {
        if self.buf.is_empty() {
            Ok(())
        } else {
            Err(DecodeError::TrailingBytes(self.buf.len()))
        }
    }
}

impl Encode for Hash {
    fn encode(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.0);
    }
}

impl Decode for Hash {
    fn decode(reader: &mut Reader<'_>) -> Result<Self, DecodeError> {
        reader.hash()
    }
}

/// Domain-separated Blake3 hash, so hashes of different object kinds can never collide.
pub fn tagged_hash(context: &str, parts: &[&[u8]]) -> Hash {
    let mut hasher = blake3::Hasher::new_derive_key(context);
    for part in parts {
        hasher.update(part);
    }
    Hash(*hasher.finalize().as_bytes())
}
