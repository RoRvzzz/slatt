use rhai::{Array, Blob, Dynamic, Engine, EvalAltResult};
use std::rc::Rc;

const MAX_PACKED_VALUES: i64 = 65_536;

#[derive(Clone, Copy)]
enum PackedField {
    Fixed(i64, bool),
    Var(u32),
}

impl PackedField {
    fn parse(name: &str) -> Result<Self, Box<EvalAltResult>> {
        Ok(match name {
            "u8" => Self::Fixed(1, false),
            "u16le" => Self::Fixed(2, false),
            "u32le" => Self::Fixed(4, false),
            "u64le" => Self::Fixed(8, false),
            "u16be" => Self::Fixed(2, true),
            "u32be" => Self::Fixed(4, true),
            "u64be" => Self::Fixed(8, true),
            "vu32" => Self::Var(32),
            "vu63" => Self::Var(63),
            _ => return Err(format!("unknown packed record field {name:?}").into()),
        })
    }

    fn read(self, cursor: &mut ByteCursor) -> Result<Dynamic, Box<EvalAltResult>> {
        Ok(match self {
            Self::Fixed(width, big_endian) => cursor.read_uint(width, big_endian)?,
            Self::Var(bits) => cursor.varuint(bits)?,
        }
        .into())
    }
}

#[derive(Clone)]
struct ByteCursor {
    data: Rc<[u8]>,
    offset: usize,
    end: usize,
}

impl ByteCursor {
    fn new(data: Blob) -> Self {
        Self {
            offset: 0,
            end: data.len(),
            data: data.into(),
        }
    }

    fn checked_end(&self, size: i64) -> Result<usize, Box<EvalAltResult>> {
        usize::try_from(size)
            .ok()
            .and_then(|size| self.offset.checked_add(size))
            .filter(|end| *end <= self.end)
            .ok_or_else(|| {
                format!(
                    "byte cursor at 0x{:x}: cannot read {size} bytes ({} remaining)",
                    self.offset,
                    self.end - self.offset
                )
                .into()
            })
    }

    fn read_uint(&mut self, size: i64, big_endian: bool) -> Result<i64, Box<EvalAltResult>> {
        if !(1..=8).contains(&size) {
            return Err(format!(
                "byte cursor at 0x{:x}: integer width must be 1–8",
                self.offset
            )
            .into());
        }
        let end = self.checked_end(size)?;
        let mut word = [0; 8];
        if big_endian {
            word[8 - size as usize..].copy_from_slice(&self.data[self.offset..end]);
        } else {
            word[..size as usize].copy_from_slice(&self.data[self.offset..end]);
        }
        self.offset = end;
        Ok(if big_endian {
            i64::from_be_bytes(word)
        } else {
            i64::from_le_bytes(word)
        })
    }

    fn varuint(&mut self, bits: u32) -> Result<i64, Box<EvalAltResult>> {
        let mut value = 0;
        for index in 0..bits.div_ceil(7) {
            let end = self.checked_end(i64::from(index + 1))?;
            let byte = self.data[end - 1];
            value |= i64::from(byte & 0x7f) << (index * 7);
            if byte < 0x80 && (value as u64) < (1u64 << bits) {
                self.offset = end;
                return Ok(value);
            }
        }
        Err(format!(
            "byte cursor at 0x{:x}: varuint exceeds {bits} bits",
            self.offset
        )
        .into())
    }

    fn take(&mut self, size: i64) -> Result<Blob, Box<EvalAltResult>> {
        let end = self.checked_end(size)?;
        let bytes = self.data[self.offset..end].to_vec();
        self.offset = end;
        Ok(bytes)
    }

    fn u32s_le(&mut self, count: i64) -> Result<Array, Box<EvalAltResult>> {
        if !(0..=MAX_PACKED_VALUES).contains(&count) {
            return Err("u32s_le count must be 0–65,536".into());
        }
        let end = self.checked_end(count * 4)?;
        let values = u32_values(&self.data[self.offset..end]);
        self.offset = end;
        Ok(values)
    }

    fn u32_rows_le(&mut self, rows: i64, width: i64) -> Result<Array, Box<EvalAltResult>> {
        let count = rows
            .checked_mul(width)
            .filter(|&count| {
                rows >= 0 && (1..=MAX_PACKED_VALUES).contains(&width) && count <= MAX_PACKED_VALUES
            })
            .ok_or(
                "u32_rows_le needs nonnegative rows, positive width, and at most 65,536 words",
            )?;
        let end = self.checked_end(count * 4)?;
        let values = self.data[self.offset..end]
            .chunks_exact(width as usize * 4)
            .map(|row| Dynamic::from_array(u32_values(row)))
            .collect();
        self.offset = end;
        Ok(values)
    }

    fn read_record(&mut self, layout: &str) -> Result<Array, Box<EvalAltResult>> {
        let mut cursor = self.clone();
        let mut fields = Array::new();
        let mut remaining = MAX_PACKED_VALUES;
        for spec in layout.split_ascii_whitespace() {
            let (name, count) = match spec.split_once('[') {
                Some((name, count)) => {
                    let count = count
                        .strip_suffix(']')
                        .and_then(|value| value.parse::<i64>().ok())
                        .filter(|count| *count >= 0)
                        .ok_or("packed record group needs a nonnegative integer count")?;
                    (name, Some(count))
                }
                None => (spec, None),
            };
            let field = PackedField::parse(name)?;
            remaining -= count.unwrap_or(1).min(MAX_PACKED_VALUES + 1);
            if remaining < 0 || fields.len() == MAX_PACKED_VALUES as usize {
                return Err("packed record exceeds 65,536 values".into());
            }
            if let Some(count) = count {
                let values = (0..count)
                    .map(|_| field.read(&mut cursor))
                    .collect::<Result<Array, _>>()?;
                fields.push(Dynamic::from_array(values));
            } else {
                fields.push(field.read(&mut cursor)?);
            }
        }
        if fields.is_empty() {
            return Err("packed record layout is empty".into());
        }
        self.offset = cursor.offset;
        Ok(fields)
    }

    fn child(&mut self, size: i64) -> Result<Self, Box<EvalAltResult>> {
        let end = self.checked_end(size)?;
        let child = Self {
            data: self.data.clone(),
            offset: self.offset,
            end,
        };
        self.offset = end;
        Ok(child)
    }
}

fn u32_values(data: &[u8]) -> Array {
    data.as_chunks::<4>()
        .0
        .iter()
        .map(|&bytes| i64::from(u32::from_le_bytes(bytes)).into())
        .collect()
}

pub fn register(engine: &mut Engine) {
    engine.register_type_with_name::<ByteCursor>("ByteCursor");
    engine.register_fn("byte_cursor", ByteCursor::new);
    engine.register_get("offset", |cursor: &mut ByteCursor| cursor.offset as i64);
    engine.register_get("remaining", |cursor: &mut ByteCursor| {
        (cursor.end - cursor.offset) as i64
    });
    engine.register_fn("read_uint", ByteCursor::read_uint);
    engine.register_fn("read_varuint", |cursor: &mut ByteCursor| cursor.varuint(63));
    engine.register_fn("vu32", |cursor: &mut ByteCursor| cursor.varuint(32));
    engine.register_fn("u8", |cursor: &mut ByteCursor| cursor.read_uint(1, false));
    engine.register_fn("u16le", |cursor: &mut ByteCursor| {
        cursor.read_uint(2, false)
    });
    engine.register_fn("u32le", |cursor: &mut ByteCursor| {
        cursor.read_uint(4, false)
    });
    engine.register_fn("u64le", |cursor: &mut ByteCursor| {
        cursor.read_uint(8, false)
    });
    engine.register_fn("u32s_le", ByteCursor::u32s_le);
    engine.register_fn("u32_rows_le", ByteCursor::u32_rows_le);
    engine.register_fn("read_record", ByteCursor::read_record);
    engine.register_fn("take", ByteCursor::take);
    engine.register_fn("substream", ByteCursor::child);
    engine.register_fn("adler32", |bytes: &mut Blob| -> i64 {
        let mut a = 1u32;
        let mut b = 0u32;
        for &byte in bytes.iter() {
            a = (a + u32::from(byte)) % 65521;
            b = (b + a) % 65521;
        }
        i64::from((b << 16) | a)
    });
    engine.register_fn("decode_base85", decode_base85);
    engine.register_fn("decode_ascii85", decode_ascii85);
}

fn decode_ascii85(data: &mut Blob) -> Result<Blob, Box<EvalAltResult>> {
    let alphabet = (b'!'..=b'u').collect();
    let tail = data.len() % 5;
    if tail == 0 {
        return decode_base85(data, alphabet, true);
    }
    if tail == 1 {
        return Err("Ascii85 tail needs two to four digits".into());
    }
    let mut padded = data.clone();
    padded.resize(data.len() + 5 - tail, b'u');
    let mut output = decode_base85(&mut padded, alphabet, true)?;
    output.truncate(output.len() - (5 - tail));
    Ok(output)
}

fn decode_base85(
    data: &mut Blob,
    alphabet: Blob,
    big_endian: bool,
) -> Result<Blob, Box<EvalAltResult>> {
    if alphabet.len() != 85 || !data.len().is_multiple_of(5) {
        return Err("Base85 requires 85 symbols and complete five-byte words".into());
    }
    let mut digits = [u8::MAX; 256];
    for (digit, symbol) in alphabet.into_iter().enumerate() {
        let slot = &mut digits[symbol as usize];
        if *slot != u8::MAX {
            return Err("duplicate Base85 alphabet symbol".into());
        }
        *slot = digit as u8;
    }
    let mut output = Vec::with_capacity(data.len() / 5 * 4);
    for (index, chunk) in data.as_chunks::<5>().0.iter().enumerate() {
        let mut word = 0u64;
        for (position, &byte) in chunk.iter().enumerate() {
            let digit = digits[byte as usize];
            if digit == u8::MAX {
                return Err(
                    format!("invalid Base85 symbol at byte {}", index * 5 + position).into(),
                );
            }
            word = word * 85 + u64::from(digit);
        }
        let word = u32::try_from(word)
            .map_err(|_| format!("Base85 word at byte {} exceeds 32 bits", index * 5))?;
        output.extend_from_slice(&if big_endian {
            word.to_be_bytes()
        } else {
            word.to_le_bytes()
        });
    }
    Ok(output)
}
