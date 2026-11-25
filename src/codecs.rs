use rhai::{Array, Blob, Engine, EvalAltResult};
use std::collections::BTreeSet;
use std::rc::Rc;

type Result<T> = std::result::Result<T, Box<EvalAltResult>>;

const MAX_CODE_BITS: usize = 24;

#[derive(Clone)]
struct BitCursor {
    data: Rc<[u8]>,
    position: usize,
}

impl BitCursor {
    fn bit(&mut self) -> Result<u32> {
        let byte = self
            .data
            .get(self.position / 8)
            .ok_or_else(|| format!("truncated bit stream at bit {}", self.position))?;
        let bit = (byte >> (self.position % 8)) & 1;
        self.position += 1;
        Ok(u32::from(bit))
    }

    fn gamma(&mut self) -> Result<u32> {
        let mut width = 0;
        while self.bit()? == 0 {
            width += 1;
            if width == MAX_CODE_BITS {
                return Err("Elias gamma value exceeds 24 bits".into());
            }
        }
        let mut value = 1;
        for _ in 0..width {
            value = (value << 1) | self.bit()?;
        }
        Ok(value)
    }

    fn read_bits(&mut self, width: i64) -> Result<i64> {
        if !(0..=32).contains(&width) {
            return Err("bit field width must be 0–32".into());
        }
        let mut reader = self.clone();
        let mut value = 0;
        for shift in 0..width {
            value |= i64::from(reader.bit()?) << shift;
        }
        self.position = reader.position;
        Ok(value)
    }

    fn rice(&mut self, width: i64, limit: i64) -> Result<i64> {
        if !(0..=24).contains(&width) || limit < 0 {
            return Err("Rice coding requires a width of 0–24 and a nonnegative limit".into());
        }
        let mut reader = self.clone();
        let mut quotient = 0;
        while reader.bit()? == 0 {
            quotient += 1;
            if quotient > (limit >> width) {
                return Err("Rice value exceeds its limit".into());
            }
        }
        let value = (quotient << width) | reader.read_bits(width)?;
        if value > limit {
            return Err("Rice value exceeds its limit".into());
        }
        self.position = reader.position;
        Ok(value)
    }

    fn mtf_runs(&mut self, table: Huffman, length: i64, escape: i64) -> Result<Blob> {
        let length = usize::try_from(length)
            .ok()
            .filter(|&length| length <= crate::frontend::MAX_INPUT_BYTES)
            .ok_or("move-to-front output exceeds the 4 MiB limit")?;
        let escape = usize::try_from(escape)
            .ok()
            .filter(|&escape| (256..table.0.alphabet).contains(&escape))
            .ok_or("move-to-front escape must be outside the byte ranks and inside the alphabet")?;
        let mut reader = self.clone();
        let mut order = std::array::from_fn::<_, 256, _>(|rank| rank as u8);
        let mut output = Vec::with_capacity(length);
        while output.len() < length {
            let symbol = table.0.symbol(&mut reader)?;
            let (rank, count) = if symbol == escape {
                (table.0.symbol(&mut reader)?, reader.gamma()? as usize)
            } else {
                (symbol, 1)
            };
            if rank >= order.len() || count > length - output.len() {
                return Err(format!(
                    "invalid move-to-front run ending at bit {}",
                    reader.position
                )
                .into());
            }
            if rank == 0 {
                output.resize(output.len() + count, order[0]);
            } else {
                for _ in 0..count {
                    output.push(order[rank]);
                    order[..=rank].rotate_right(1);
                }
            }
        }
        self.position = reader.position;
        Ok(output)
    }
}

#[derive(Clone)]
struct Huffman(Rc<Codes>);

struct Codes {
    alphabet: usize,
    first: [u32; MAX_CODE_BITS + 1],
    offsets: [usize; MAX_CODE_BITS + 2],
    symbols: Vec<usize>,
    max_width: usize,
}

#[derive(Clone)]
struct PrefixHuffman(Rc<[PrefixNode]>);

#[derive(Default)]
struct PrefixNode {
    children: [Option<usize>; 2],
    symbol: Option<i64>,
}

fn prefix_huffman(entries: Array) -> Result<PrefixHuffman> {
    if entries.is_empty() || entries.len() > 65_536 {
        return Err("prefix codebook needs 1–65,536 entries".into());
    }
    let mut nodes = vec![PrefixNode::default()];
    let mut used = BTreeSet::new();
    for entry in entries {
        let entry = entry
            .try_cast::<Array>()
            .ok_or("prefix codebook entry needs an array")?;
        let [symbol, width, code] = entry.as_slice() else {
            return Err("prefix codebook entry needs [symbol, width, code]".into());
        };
        let symbol = symbol
            .as_int()
            .map_err(|_| "prefix symbol must be an integer")?;
        let width = width
            .as_int()
            .map_err(|_| "prefix width must be an integer")?;
        let code = code
            .as_int()
            .map_err(|_| "prefix code must be an integer")?;
        if !(1..=MAX_CODE_BITS as i64).contains(&width) || !(0..1 << width).contains(&code) {
            return Err("prefix code does not fit a width of 1–24 bits".into());
        }
        if !used.insert(symbol) {
            return Err("duplicate prefix symbol".into());
        }
        let mut at = 0;
        for shift in (0..width).rev() {
            if nodes[at].symbol.is_some() {
                return Err("prefix code overlaps a shorter code".into());
            }
            let bit = ((code >> shift) & 1) as usize;
            at = match nodes[at].children[bit] {
                Some(index) => index,
                None => {
                    let index = nodes.len();
                    nodes[at].children[bit] = Some(index);
                    nodes.push(PrefixNode::default());
                    index
                }
            };
        }
        if nodes[at].symbol.is_some() || nodes[at].children != [None; 2] {
            return Err("prefix code overlaps another code".into());
        }
        nodes[at].symbol = Some(symbol);
    }
    Ok(PrefixHuffman(nodes.into()))
}

fn read_prefix(bits: &mut BitCursor, table: PrefixHuffman) -> Result<i64> {
    let mut reader = bits.clone();
    let mut at = 0;
    loop {
        let bit = reader.bit()? as usize;
        at = table.0[at].children[bit]
            .ok_or_else(|| format!("invalid prefix code ending at bit {}", reader.position))?;
        if let Some(symbol) = table.0[at].symbol {
            bits.position = reader.position;
            return Ok(symbol);
        }
    }
}

impl Codes {
    fn symbol(&self, reader: &mut BitCursor) -> Result<usize> {
        let mut code = 0;
        for width in 1..=self.max_width {
            code = (code << 1) | reader.bit()?;
            let offset = self.offsets[width];
            if let Some(index) = code.checked_sub(self.first[width])
                && (index as usize) < self.offsets[width + 1] - offset
            {
                return Ok(self.symbols[offset + index as usize]);
            }
        }
        Err(format!("invalid Huffman code ending at bit {}", reader.position).into())
    }
}

fn canonical_huffman(lengths: &mut Blob) -> Result<Huffman> {
    if lengths.len() > 65_536 {
        return Err("Huffman alphabet exceeds 65,536 symbols".into());
    }
    let max_width = usize::from(lengths.iter().copied().max().unwrap_or(0));
    if !(1..=MAX_CODE_BITS).contains(&max_width) {
        return Err("Huffman code widths must be 1–24 bits (0 excludes a symbol)".into());
    }
    let mut counts = [0u32; MAX_CODE_BITS + 1];
    for &length in lengths.iter().filter(|&&length| length != 0) {
        counts[usize::from(length)] += 1;
    }
    let mut first = [0; MAX_CODE_BITS + 1];
    let mut offsets = [0; MAX_CODE_BITS + 2];
    for width in 1..=max_width {
        first[width] = (first[width - 1] + counts[width - 1]) << 1;
        if first[width] + counts[width] > 1 << width {
            return Err(format!("oversubscribed Huffman codes at width {width}").into());
        }
        offsets[width + 1] = offsets[width] + counts[width] as usize;
    }
    let mut symbols = vec![0; offsets[max_width + 1]];
    let mut next = offsets;
    for (symbol, &length) in lengths
        .iter()
        .enumerate()
        .filter(|&(_, &length)| length != 0)
    {
        let index = &mut next[usize::from(length)];
        symbols[*index] = symbol;
        *index += 1;
    }
    Ok(Huffman(Rc::new(Codes {
        alphabet: lengths.len(),
        first,
        offsets,
        symbols,
        max_width,
    })))
}

fn inverse_bwt(column: &mut Blob, row: i64) -> Result<Blob> {
    let mut row = usize::try_from(row)
        .ok()
        .filter(|&row| row < column.len())
        .ok_or("BWT row is outside the last column")?;
    if column.len() > crate::frontend::MAX_INPUT_BYTES {
        return Err("BWT column exceeds 4 MiB".into());
    }
    let mut counts = [0; 256];
    for &byte in column.iter() {
        counts[usize::from(byte)] += 1;
    }
    let mut offset = 0;
    for count in &mut counts {
        let start = offset;
        offset += *count;
        *count = start;
    }
    let mut links = vec![0; column.len()];
    for (index, &byte) in column.iter().enumerate() {
        let offset = &mut counts[usize::from(byte)];
        links[*offset] = index;
        *offset += 1;
    }
    let mut output = Vec::with_capacity(column.len());
    for _ in 0..column.len() {
        row = links[row];
        output.push(column[row]);
    }
    Ok(output)
}

fn decode_mtf(ranks: &mut Blob, context_bits: i64) -> Result<Blob> {
    if ranks.len() > crate::frontend::MAX_INPUT_BYTES {
        return Err("move-to-front input exceeds 4 MiB".into());
    }
    if !(0..=8).contains(&context_bits) {
        return Err("move-to-front context width must be 0–8 bits".into());
    }
    let alphabet = std::array::from_fn::<_, 256, _>(|rank| rank as u8);
    let mut lists = vec![alphabet; 1 << context_bits];
    let mut context = 0;
    let mut output = Vec::with_capacity(ranks.len());
    for &rank in ranks.iter() {
        let list = &mut lists[context];
        let rank = usize::from(rank);
        let byte = list[rank];
        list[..=rank].rotate_right(1);
        output.push(byte);
        context = usize::from(byte) >> (8 - context_bits);
    }
    Ok(output)
}

fn append_byte(output: &mut Blob, byte: i64, count: i64) -> Result<()> {
    let byte = u8::try_from(byte).map_err(|_| "run byte must be 0–255")?;
    let end = usize::try_from(count)
        .ok()
        .and_then(|count| output.len().checked_add(count))
        .filter(|&end| end <= crate::frontend::MAX_INPUT_BYTES)
        .ok_or("byte run exceeds the 4 MiB blob limit")?;
    output.resize(end, byte);
    Ok(())
}

pub fn register(engine: &mut Engine) {
    engine.register_type_with_name::<BitCursor>("BitCursor");
    engine.register_type_with_name::<Huffman>("CanonicalHuffman");
    engine.register_type_with_name::<PrefixHuffman>("PrefixHuffman");
    engine.register_fn("bit_cursor_lsb", |data: Blob| BitCursor {
        data: data.into(),
        position: 0,
    });
    engine.register_get("remaining", |bits: &mut BitCursor| {
        (bits.data.len() * 8 - bits.position) as i64
    });
    engine.register_fn("read_gamma", |bits: &mut BitCursor| -> Result<i64> {
        let mut reader = bits.clone();
        let value = reader.gamma()?;
        bits.position = reader.position;
        Ok(i64::from(value))
    });
    engine.register_fn("read_bits", BitCursor::read_bits);
    engine.register_fn("read_rice", BitCursor::rice);
    engine.register_fn("canonical_huffman", canonical_huffman);
    engine.register_fn("prefix_huffman", prefix_huffman);
    engine.register_fn("read_huffman", read_prefix);
    engine.register_fn(
        "read_huffman",
        |bits: &mut BitCursor, table: Huffman| -> Result<i64> {
            let mut reader = bits.clone();
            let symbol = table.0.symbol(&mut reader)?;
            bits.position = reader.position;
            Ok(symbol as i64)
        },
    );
    engine.register_fn("read_mtf_runs", BitCursor::mtf_runs);
    engine.register_fn("decode_mtf", decode_mtf);
    engine.register_fn("append_byte", append_byte);
    engine.register_fn("inverse_bwt", inverse_bwt);
}
