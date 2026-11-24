mod lua51;
mod luau;

use crate::ir::{Block, Const, Inst, Op, Operand, Proto, Reg};
use std::collections::BTreeSet;

pub fn import(bytes: &[u8]) -> Result<Vec<Proto>, String> {
    if bytes.starts_with(b"\x1bLua") {
        lua51::import(bytes)
    } else {
        luau::import(bytes)
    }
}

struct Reader<'a> {
    input: &'a [u8],
    position: usize,
    big_endian: bool,
}

impl<'a> Reader<'a> {
    fn new(input: &'a [u8]) -> Self {
        Self {
            input,
            position: 0,
            big_endian: false,
        }
    }

    fn error(&self, message: &str) -> String {
        format!("byte {}: {message}", self.position)
    }

    fn take(&mut self, size: usize) -> Result<&'a [u8], String> {
        let end = self
            .position
            .checked_add(size)
            .ok_or_else(|| self.error("invalid field size"))?;
        let data = self
            .input
            .get(self.position..end)
            .ok_or_else(|| self.error("truncated chunk"))?;
        self.position = end;
        Ok(data)
    }

    fn byte(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }

    fn boolean(&mut self) -> Result<bool, String> {
        match self.byte()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(self.error("invalid boolean")),
        }
    }

    fn word(&mut self) -> Result<u32, String> {
        let bytes = self.take(4)?.try_into().unwrap();
        Ok(if self.big_endian {
            u32::from_be_bytes(bytes)
        } else {
            u32::from_le_bytes(bytes)
        })
    }

    fn wide(&mut self) -> Result<u64, String> {
        let bytes = self.take(8)?.try_into().unwrap();
        Ok(if self.big_endian {
            u64::from_be_bytes(bytes)
        } else {
            u64::from_le_bytes(bytes)
        })
    }

    fn var(&mut self, bits: u32) -> Result<u64, String> {
        let mut value = 0;
        for offset in (0..bits).step_by(7) {
            let byte = self.byte()?;
            let payload = u64::from(byte & 127);
            if payload >= (1 << (bits - offset).min(7)) {
                break;
            }
            value |= payload << offset;
            if byte < 128 {
                return Ok(value);
            }
        }
        Err(self.error("invalid variable-length integer"))
    }

    fn var32(&mut self) -> Result<usize, String> {
        self.var(32).map(|value| value as usize)
    }

    fn count(&self, count: usize, width: usize) -> Result<usize, String> {
        if count > (self.input.len() - self.position) / width {
            return Err(self.error("declared array exceeds chunk size"));
        }
        Ok(count)
    }

    fn words(&mut self, count: usize) -> Result<Vec<u32>, String> {
        let count = self.count(count, 4)?;
        (0..count).map(|_| self.word()).collect()
    }

    fn end(&self) -> Result<(), String> {
        if self.position != self.input.len() {
            return Err(self.error("unexpected trailing bytes"));
        }
        Ok(())
    }
}

#[derive(Clone)]
enum PoolValue {
    Literal(Const),
    Native(String),
    Closure(usize),
}

struct Image {
    stack: u16,
    upvalues: usize,
    words: Vec<u32>,
    constants: Vec<PoolValue>,
}

impl Image {
    fn reg(&self, reg: u32) -> Result<Operand, String> {
        if reg >= u32::from(self.stack) {
            return Err(format!("register r{reg} is outside the frame"));
        }
        Ok(Operand::Reg(Reg(reg as u16)))
    }

    fn constant(&self, index: u32) -> Result<&PoolValue, String> {
        self.constants
            .get(index as usize)
            .ok_or_else(|| format!("constant {index} is outside the pool"))
    }

    fn literal(&self, index: u32) -> Result<Option<Const>, String> {
        Ok(match self.constant(index)? {
            PoolValue::Literal(value) => Some(value.clone()),
            PoolValue::Native(_) | PoolValue::Closure(_) => None,
        })
    }

    fn return_fixed(&self, start: u32, count: u32) -> Result<Inst, String> {
        let args = (start..start + count)
            .map(|reg| self.reg(reg))
            .collect::<Result<_, _>>()?;
        Ok(inst(Op::Return, args))
    }
}

struct Lowered {
    pc: usize,
    instructions: Vec<Inst>,
}

fn inst(op: Op, args: Vec<Operand>) -> Inst {
    Inst { op, args }
}

fn load(reg: Operand, value: Const) -> Inst {
    inst(Op::LoadK, vec![reg, Operand::Const(value)])
}

fn number(value: impl Into<f64>) -> Const {
    Const::Number(value.into())
}

fn target(pc: usize, offset: i32) -> Result<usize, String> {
    usize::try_from(pc as i64 + 1 + i64::from(offset)).map_err(|_| "negative jump target".into())
}

fn label(pc: usize) -> Operand {
    Operand::Block(format!("pc{pc}"))
}

fn jump(pc: usize) -> Inst {
    inst(Op::Jump, vec![label(pc)])
}

fn branch(op: Op, mut operands: Vec<Operand>, yes: usize, no: usize) -> Inst {
    operands.extend([label(yes), label(no)]);
    inst(op, operands)
}

fn native_args(dialect: &str, name: &str, words: &[u32]) -> Vec<Operand> {
    let tag = Operand::Const(Const::String(format!("{dialect}.{name}").into_bytes()));
    std::iter::once(tag)
        .chain(words.iter().map(|word| Operand::Const(number(*word))))
        .collect()
}

fn add_pool_value(args: &mut Vec<Operand>, image: &Image, index: u32) -> Result<(), String> {
    let value = match image.constant(index)? {
        PoolValue::Literal(value) => value.clone(),
        PoolValue::Native(description) => Const::String(description.as_bytes().to_vec()),
        PoolValue::Closure(id) => Const::String(format!("proto{id}").into_bytes()),
    };
    args.push(Operand::Const(value));
    Ok(())
}

fn assemble(name: String, lowered: Vec<Lowered>) -> Result<Proto, String> {
    let pcs: BTreeSet<_> = lowered.iter().map(|item| item.pc).collect();
    let mut leaders = BTreeSet::from([0]);
    for (i, item) in lowered.iter().enumerate() {
        if let Some(tail) = item.instructions.last().filter(|inst| inst.op.terminates()) {
            if let Some(next) = lowered.get(i + 1) {
                leaders.insert(next.pc);
            }
            for operand in &tail.args {
                if let Operand::Block(label) = operand {
                    let pc: usize = label[2..].parse().unwrap();
                    if !pcs.contains(&pc) {
                        return Err(format!(
                            "{name} pc {} jumps outside instruction boundaries to pc {pc}",
                            item.pc
                        ));
                    }
                    leaders.insert(pc);
                }
            }
        }
    }
    let mut blocks = Vec::new();
    let mut block = Block {
        name: "pc0".into(),
        instructions: Vec::new(),
    };
    for item in lowered {
        if item.pc != 0 && leaders.contains(&item.pc) {
            if !block
                .instructions
                .last()
                .is_some_and(|last| last.op.terminates())
            {
                block.instructions.push(jump(item.pc));
            }
            blocks.push(block);
            block = Block {
                name: format!("pc{}", item.pc),
                instructions: Vec::new(),
            };
        }
        block.instructions.extend(item.instructions);
    }
    blocks.push(block);
    let proto = Proto { name, blocks };
    proto
        .verify()
        .map_err(|err| format!("{}: {err}", proto.name))?;
    Ok(proto)
}
