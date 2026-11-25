use crate::ir::{Const, Facts, Inst, Op, Operand, Proto, Reg, same_fact};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Dialect {
    Lua51,
    Luau,
}

impl Dialect {
    pub fn parse(name: &str) -> Result<Self, String> {
        match name {
            "lua51" => Ok(Self::Lua51),
            "luau" => Ok(Self::Luau),
            _ => Err(format!(
                "unknown bytecode format {name}; expected lua51 or luau"
            )),
        }
    }

    fn max_frame(self) -> u32 {
        match self {
            Self::Lua51 => 250,
            Self::Luau => 255,
        }
    }

    fn abc(self, op: u32, a: u32, b: u32, c: u32) -> u32 {
        match self {
            Self::Lua51 => op | (a << 6) | (b << 23) | (c << 14),
            Self::Luau => op | (a << 8) | (b << 16) | (c << 24),
        }
    }

    fn jump(self, offset: i64) -> Result<u32, String> {
        match self {
            Self::Lua51 if (-131071..=131071).contains(&offset) => {
                Ok(22 | ((offset + 131071) as u32) << 14)
            }
            Self::Luau if (-(1 << 23)..(1 << 23)).contains(&offset) => {
                Ok(67 | ((offset as u32 & 0xff_ffff) << 8))
            }
            _ => Err(format!(
                "jump displacement {offset} is outside the target instruction encoding"
            )),
        }
    }

    fn opcode(self, lua51: u32, luau: u32) -> u32 {
        match self {
            Self::Lua51 => lua51,
            Self::Luau => luau,
        }
    }
}

pub fn emit(dialect: Dialect, program: &[Proto]) -> Result<Vec<u8>, String> {
    let bytes = match dialect {
        Dialect::Lua51 => {
            let mut bytes = b"\x1bLua\x51\x00\x01\x04\x08\x04\x08\x00".to_vec();
            let mut functions = program.iter();
            let root = functions.next().ok_or("missing root prototype")?;
            lua51(&mut bytes, root, &mut functions)?;
            if functions.next().is_some() {
                return Err("additional prototypes need an imported Lua 5.1 prototype tree".into());
            }
            bytes
        }
        Dialect::Luau => {
            let [proto] = program else {
                return Err("Luau output currently requires a single portable prototype".into());
            };
            let chunk = encode(dialect, proto)?;
            luau(proto, chunk.frame as u8, &chunk.words, &chunk.constants)?
        }
    };
    crate::binary::import(&bytes)
        .map_err(|err| format!("emitted bytecode failed structural check: {err}"))?;
    Ok(bytes)
}

fn encode(dialect: Dialect, proto: &Proto) -> Result<Encoder<'_>, String> {
    proto.verify()?;
    let registers = proto
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .flat_map(|inst| &inst.args)
        .filter_map(|arg| match arg {
            Operand::Reg(Reg(reg)) => Some(u32::from(*reg) + 1),
            _ => None,
        })
        .max()
        .unwrap_or(0);
    let origin = proto.lua51.as_ref().filter(|_| dialect == Dialect::Lua51);
    // Opaque operations can use registers not present as typed operands in the IR.
    let base = registers
        .max(u32::from(proto.params))
        .max(origin.map_or(2, |image| u32::from(image.stack)));
    if base > dialect.max_frame() {
        return Err(format!(
            "register frame needs {base} slots, target supports {}",
            dialect.max_frame()
        ));
    }
    let facts = proto.facts()?;
    let mut chunk = Encoder {
        dialect,
        base,
        frame: base,
        words: Vec::new(),
        constants: origin.map_or_else(Vec::new, |image| image.constants.clone()),
        labels: BTreeMap::new(),
        jumps: Vec::new(),
    };
    if dialect == Dialect::Luau && proto.variadic {
        chunk.abc(65, u32::from(proto.params), 0, 0);
    }
    for (block, facts) in proto.blocks.iter().zip(&facts) {
        chunk.labels.insert(block.name.as_str(), chunk.words.len());
        let mut index = 0;
        while let Some(inst) = block.instructions.get(index) {
            let emitted = if let Some(origin) = origin.filter(|_| inst.op.native()) {
                chunk.preserve_lua51(origin, inst, block.instructions.get(index + 1))
            } else {
                chunk.lower(inst, facts[index].as_ref()).map(|()| 1)
            };
            index +=
                emitted.map_err(|err| format!("{} @{}:{index}: {err}", proto.name, block.name))?;
        }
    }
    if dialect == Dialect::Lua51 && chunk.words.last().is_some_and(|word| word & 63 != 30) {
        // Lua 5.1's loader requires the final word to be a RETURN.
        chunk.abc(30, 0, 1, 0);
    }
    for (pc, label, loop_fields) in chunk.jumps.drain(..) {
        let to = chunk.labels[label];
        let jump = dialect
            .jump(to as i64 - pc as i64 - 1)
            .map_err(|err| format!("jump to @{label}: {err}"))?;
        chunk.words[pc] = loop_fields.map_or(jump, |fields| (jump & !0x3fff) | fields);
    }
    Ok(chunk)
}

struct Encoder<'a> {
    dialect: Dialect,
    base: u32,
    frame: u32,
    words: Vec<u32>,
    constants: Vec<Const>,
    labels: BTreeMap<&'a str, usize>,
    jumps: Vec<(usize, &'a str, Option<u32>)>,
}

fn reg(operand: &Operand) -> u32 {
    let Operand::Reg(Reg(reg)) = operand else {
        unreachable!("typed register operand")
    };
    u32::from(*reg)
}

impl<'a> Encoder<'a> {
    fn abc(&mut self, op: u32, a: u32, b: u32, c: u32) {
        self.words.push(self.dialect.abc(op, a, b, c));
    }

    fn jump_to(&mut self, operand: &'a Operand) {
        self.branch_to(operand, None);
    }

    fn branch_to(&mut self, operand: &'a Operand, loop_fields: Option<u32>) {
        let Operand::Block(label) = operand else {
            unreachable!("typed block operand")
        };
        self.jumps.push((self.words.len(), label, loop_fields));
        self.words.push(0);
    }

    fn preserve_lua51(
        &mut self,
        origin: &crate::binary::Lua51Origin,
        inst: &'a Inst,
        next: Option<&'a Inst>,
    ) -> Result<usize, String> {
        if !origin.opaque.contains(&inst.to_string()) {
            return Err("opaque Lua 5.1 records must come unchanged from the input chunk".into());
        }
        let Operand::Const(Const::String(tag)) = &inst.args[0] else {
            return Err("invalid native tag".into());
        };
        if tag == b"lua51.LEGACY_ARG" {
            return Ok(1);
        }
        let raw = |value: &Operand| match value {
            Operand::Const(Const::Number(value))
                if value.fract() == 0.0 && (0.0..=u32::MAX as f64).contains(value) =>
            {
                Some(*value as u32)
            }
            _ => None,
        };
        let word = inst
            .args
            .get(1)
            .and_then(raw)
            .ok_or("missing native Lua 5.1 instruction word")?;
        match word & 63 {
            27 | 33 => {
                self.words.push(word);
                self.jump_to(&inst.args[2]);
                self.jump_to(&inst.args[3]);
            }
            29 => {
                self.words.push(word);
                // A tail call of a native function falls through to an open RETURN.
                self.abc(30, (word >> 6) & 255, 0, 0);
            }
            31 => {
                self.branch_to(&inst.args[2], Some(word & 0x3fff));
                self.jump_to(&inst.args[3]);
            }
            32 => {
                let next = next
                    .filter(|next| next.op == Op::Jump)
                    .ok_or("FORPREP needs its following jump")?;
                self.branch_to(&next.args[0], Some(word & 0x3fff));
                return Ok(2);
            }
            34 if (word >> 14) & 511 == 0 => {
                self.words.push(word);
                self.words.push(
                    inst.args
                        .get(2)
                        .and_then(raw)
                        .ok_or("missing extended SETLIST word")?,
                );
            }
            36 => {
                self.words.extend(inst.args[1..].iter().map_while(raw));
            }
            _ => self.words.push(word),
        }
        Ok(1)
    }

    fn constant_index(&mut self, constant: &Const) -> usize {
        self.constants
            .iter()
            .position(|value| same_fact(value, constant))
            .unwrap_or_else(|| {
                self.constants.push(constant.clone());
                self.constants.len() - 1
            })
    }

    fn load(&mut self, dest: u32, constant: &Const) -> Result<(), String> {
        let index = self.constant_index(constant);
        match self.dialect {
            Dialect::Lua51 if index <= 0x3_ffff => {
                self.words.push(1 | (dest << 6) | (index as u32) << 14)
            }
            Dialect::Luau if index <= i16::MAX as usize => {
                self.words.push(5 | (dest << 8) | (index as u32) << 16)
            }
            Dialect::Luau if index < 1 << 23 => {
                self.abc(66, dest, 0, 0);
                self.words.push(index as u32);
            }
            _ => return Err("constant pool exceeds target LOADK range".into()),
        }
        Ok(())
    }

    fn move_reg(&mut self, dst: u32, src: u32) {
        self.abc(self.dialect.opcode(0, 6), dst, src, 0);
    }

    fn get_global(&mut self, target: u32, name: &Const) -> Result<(), String> {
        let index = self.constant_index(name);
        match self.dialect {
            Dialect::Lua51 if index <= 0x3_ffff => {
                self.words.push(5 | (target << 6) | (index as u32) << 14)
            }
            Dialect::Luau => {
                self.abc(7, target, 0, 0);
                self.words.push(
                    u32::try_from(index).map_err(|_| "global name index exceeds Luau range")?,
                );
            }
            _ => return Err("global name index exceeds Lua 5.1 range".into()),
        }
        Ok(())
    }

    fn call(&mut self, instruction: &Inst) -> Result<(), String> {
        let (Reg(function), results, args) = instruction.call_parts();
        let argument_width = args.len() as u32 + 1;
        let result_width = results.len() as u32 + 1;
        let end = self.base + argument_width.max(result_width);
        if end > self.dialect.max_frame() {
            return Err(format!("call needs {end} target register slots"));
        }
        self.frame = self.frame.max(end);
        self.move_reg(self.base, u32::from(function));
        for (index, argument) in args.iter().enumerate() {
            self.move_reg(self.base + 1 + index as u32, reg(argument));
        }
        self.abc(
            self.dialect.opcode(28, 21),
            self.base,
            argument_width,
            result_width,
        );
        for (index, result) in results.iter().enumerate() {
            self.move_reg(reg(result), self.base + index as u32);
        }
        Ok(())
    }

    fn fixed_range(&mut self, inst: &Inst) -> Result<(), String> {
        let registers: Vec<u32> = inst.args.iter().map(reg).collect();
        let count = u32::try_from(registers.len()).map_err(|_| "too many fixed values")?;
        let is_return = inst.op == Op::Return;
        if !is_return && count == 0 {
            return Ok(());
        }
        if count > self.dialect.opcode(250, 254) {
            return Err("too many fixed values for the target bytecode register frame".into());
        }
        let first = registers.first().copied().unwrap_or(0);
        let contiguous = registers.iter().copied().eq(first..first + count);
        let start = if contiguous {
            first
        } else {
            let end = self.base + count;
            if end > self.dialect.max_frame() {
                return Err(format!(
                    "nonconsecutive values need {end} target register slots"
                ));
            }
            self.frame = self.frame.max(end);
            if is_return {
                for (index, reg) in registers.iter().enumerate() {
                    self.move_reg(self.base + index as u32, *reg);
                }
            }
            self.base
        };
        let opcode = if is_return {
            self.dialect.opcode(30, 22)
        } else {
            self.dialect.opcode(37, 63)
        };
        self.abc(opcode, start, count + 1, 0);
        if !is_return && !contiguous {
            for (index, reg) in registers.into_iter().enumerate() {
                self.move_reg(reg, self.base + index as u32);
            }
        }
        Ok(())
    }

    fn conditional(&mut self, inst: &'a Inst) {
        let target = inst.args.len() - 2;
        let (yes, no) = (&inst.args[target], &inst.args[target + 1]);
        let left = reg(&inst.args[0]);
        let right = (inst.op != Op::Branch).then(|| reg(&inst.args[1]));
        match self.dialect {
            Dialect::Lua51 => {
                match right {
                    Some(right) => {
                        let op = match inst.op {
                            Op::BranchEq => 23,
                            Op::BranchLt => 24,
                            _ => 25,
                        };
                        self.abc(op, 1, left, right);
                    }
                    None => self.abc(26, left, 0, 1),
                }
                self.jump_to(yes);
                self.jump_to(no);
            }
            Dialect::Luau => {
                // Branch over the false JUMPX to the true JUMPX, with AUX where required.
                match right {
                    Some(right) => {
                        let op = match inst.op {
                            Op::BranchEq => 27,
                            Op::BranchLt => 29,
                            _ => 28,
                        };
                        self.abc(op, left, 2, 0);
                        self.words.push(right);
                    }
                    None => self.abc(25, left, 1, 0),
                }
                self.jump_to(no);
                self.jump_to(yes);
            }
        }
    }

    fn lower(&mut self, inst: &'a Inst, facts: Option<&Facts>) -> Result<(), String> {
        match inst.op {
            Op::Nop => {}
            Op::LoadK => {
                let Operand::Const(constant) = &inst.args[1] else {
                    unreachable!("typed constant operand")
                };
                self.load(reg(&inst.args[0]), constant)?;
            }
            Op::GetGlobal => {
                let Operand::Const(name) = &inst.args[1] else {
                    unreachable!("typed global name")
                };
                self.get_global(reg(&inst.args[0]), name)?;
            }
            Op::Call => self.call(inst)?,
            Op::Closure
            | Op::CloseUpvalues
            | Op::ForNext
            | Op::CallOpen
            | Op::CallFromTop
            | Op::ReturnTop
            | Op::TailCall
            | Op::JumpTable
            | Op::SelfCall
            | Op::SetList
            | Op::GetUpvalue
            | Op::SetUpvalue
            | Op::NewTable
            | Op::GetTable
            | Op::SetTable
            | Op::Not
            | Op::Len
            | Op::Pow
            | Op::Concat
            | Op::Eq
            | Op::Ne
            | Op::Lt => {
                return Err(format!(
                    "{} currently requires Lua source emission",
                    inst.op.name()
                ));
            }
            Op::Move => self.move_reg(reg(&inst.args[0]), reg(&inst.args[1])),
            Op::Add | Op::Sub | Op::Mul | Op::Div => {
                let offset = match inst.op {
                    Op::Add => 0,
                    Op::Sub => 1,
                    Op::Mul => 2,
                    _ => 3,
                };
                self.abc(
                    self.dialect.opcode(12, 33) + offset,
                    reg(&inst.args[0]),
                    reg(&inst.args[1]),
                    reg(&inst.args[2]),
                );
            }
            Op::Xor32 => {
                let constant = facts
                    .and_then(|facts| inst.result(facts))
                    .ok_or("XOR32 needs a known result for native emission")?;
                self.load(reg(&inst.args[0]), &constant)?;
            }
            Op::Jump => self.jump_to(&inst.args[0]),
            Op::Branch | Op::BranchEq | Op::BranchLt | Op::BranchLe => self.conditional(inst),
            Op::Return | Op::Vararg => self.fixed_range(inst)?,
            Op::Native | Op::NativeBranch | Op::NativeReturn => {
                return Err(format!("cannot emit opaque instruction {inst}"));
            }
        }
        Ok(())
    }
}

fn word(out: &mut Vec<u8>, value: u32) {
    out.extend(value.to_le_bytes());
}

fn int_count(out: &mut Vec<u8>, count: usize) -> Result<(), String> {
    let count =
        i32::try_from(count).map_err(|_| "Lua chunk count exceeds the signed 32-bit format")?;
    out.extend(count.to_le_bytes());
    Ok(())
}

fn lua51<'a>(
    out: &mut Vec<u8>,
    proto: &'a Proto,
    remaining: &mut impl Iterator<Item = &'a Proto>,
) -> Result<(), String> {
    let chunk = encode(Dialect::Lua51, proto)?;
    let upvalues = proto.lua51.as_ref().map_or(0, |image| image.upvalues);
    let varargs = proto
        .lua51
        .as_ref()
        .map_or(if proto.variadic { 2 } else { 0 }, |image| {
            image.vararg_flags
        });
    let children = proto
        .lua51
        .as_ref()
        .map_or(&[][..], |image| image.children.as_slice());
    out.extend([0; 16]); // source name (size_t) and source line range
    out.extend([upvalues, proto.params, varargs, chunk.frame as u8]);
    int_count(out, chunk.words.len())?;
    for instruction in chunk.words {
        word(out, instruction);
    }
    int_count(out, chunk.constants.len())?;
    for constant in &chunk.constants {
        match constant {
            Const::Nil => out.push(0),
            Const::Bool(value) => out.extend([1, u8::from(*value)]),
            Const::Number(value) => {
                out.push(3);
                out.extend(value.to_le_bytes());
            }
            Const::String(bytes) => {
                out.push(4);
                out.extend((bytes.len() as u64 + 1).to_le_bytes());
                out.extend(bytes);
                out.push(0);
            }
        }
    }
    int_count(out, children.len())?;
    for name in children {
        let child = remaining
            .next()
            .filter(|child| &child.name == name)
            .ok_or_else(|| format!("missing Lua 5.1 child prototype {name}"))?;
        lua51(out, child, remaining)?;
    }
    out.extend([0; 12]); // three debug tables
    Ok(())
}

fn var_count(out: &mut Vec<u8>, count: usize) -> Result<(), String> {
    let mut count = u32::try_from(count).map_err(|_| "Luau chunk count exceeds 32-bit limits")?;
    while count >= 128 {
        out.push((count as u8 & 127) | 128);
        count >>= 7;
    }
    out.push(count as u8);
    Ok(())
}

fn luau(proto: &Proto, frame: u8, words: &[u32], constants: &[Const]) -> Result<Vec<u8>, String> {
    let mut body = vec![frame, proto.params, 0, u8::from(proto.variadic), 0, 0]; // flags and empty typemap
    var_count(&mut body, words.len())?;
    for &instruction in words {
        word(&mut body, instruction);
    }
    var_count(&mut body, constants.len())?;
    let mut strings = Vec::new();
    for constant in constants {
        match constant {
            Const::Nil => body.push(0),
            Const::Bool(value) => body.extend([1, u8::from(*value)]),
            Const::Number(value) => {
                body.push(2);
                body.extend(value.to_le_bytes());
            }
            Const::String(bytes) => {
                strings.push(bytes);
                body.push(3);
                var_count(&mut body, strings.len())?;
            }
        }
    }
    body.extend([0; 6]); // children, defined line, debug name, line info, locals, call feedback
    let mut out = vec![14, 3];
    var_count(&mut out, strings.len())?;
    for string in strings {
        var_count(&mut out, string.len())?;
        out.extend(string);
    }
    out.extend([0, 1]); // userdata type remapping terminator and one prototype
    var_count(&mut out, body.len())?;
    out.extend(body);
    out.push(0); // main prototype id
    Ok(out)
}
