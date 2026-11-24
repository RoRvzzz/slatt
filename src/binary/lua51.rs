use super::*;

struct Function {
    image: Image,
    children: Vec<Function>,
    vararg_flags: u8,
}

const OPS: &[&str] = &[
    "MOVE",
    "LOADK",
    "LOADBOOL",
    "LOADNIL",
    "GETUPVAL",
    "GETGLOBAL",
    "GETTABLE",
    "SETGLOBAL",
    "SETUPVAL",
    "SETTABLE",
    "NEWTABLE",
    "SELF",
    "ADD",
    "SUB",
    "MUL",
    "DIV",
    "MOD",
    "POW",
    "UNM",
    "NOT",
    "LEN",
    "CONCAT",
    "JMP",
    "EQ",
    "LT",
    "LE",
    "TEST",
    "TESTSET",
    "CALL",
    "TAILCALL",
    "RETURN",
    "FORLOOP",
    "FORPREP",
    "TFORLOOP",
    "SETLIST",
    "CLOSE",
    "CLOSURE",
    "VARARG",
];

pub(super) fn import(bytes: &[u8]) -> Result<Vec<Proto>, String> {
    let mut r = Reader::new(bytes);
    let header = r.take(12)?;
    if header[4] != 0x51 || header[5] != 0 {
        return Err(format!(
            "expected stock Lua 5.1 bytecode, got version byte 0x{:02x} and format {}",
            header[4], header[5]
        ));
    }
    let [endian, int, size_t, word, number, integral] = header[6..12] else {
        unreachable!()
    };
    if endian > 1
        || int != 4
        || !matches!(size_t, 4 | 8)
        || word != 4
        || number != 8
        || integral != 0
    {
        return Err(
            "unsupported Lua 5.1 ABI (requires 32-bit int/instructions and double numbers)".into(),
        );
    }
    r.big_endian = endian == 0;
    let function = read_function(&mut r, size_t, 0)?;
    r.end()?;
    let mut program = Vec::new();
    lower_tree(&mut program, &function, "main".into())?;
    Ok(program)
}

fn string<'a>(r: &mut Reader<'a>, size_t: u8) -> Result<Option<&'a [u8]>, String> {
    let size = if size_t == 4 {
        u64::from(r.word()?)
    } else {
        r.wide()?
    };
    let size = usize::try_from(size).map_err(|_| r.error("invalid string length"))?;
    if size == 0 {
        return Ok(None);
    }
    let data = r.take(size)?;
    data.strip_suffix(&[0])
        .map(Some)
        .ok_or_else(|| r.error("Lua string lacks a final zero byte"))
}

fn read_function(r: &mut Reader<'_>, size_t: u8, depth: usize) -> Result<Function, String> {
    if depth >= 200 {
        return Err(r.error("Lua prototype nesting exceeds 200"));
    }
    string(r, size_t)?;
    r.take(8)?;
    let upvalues = usize::from(r.byte()?);
    let params = r.byte()?;
    let vararg_flags = r.byte()?;
    let stack = u16::from(r.byte()?);
    if stack == 0 || u16::from(params) > stack {
        return Err(r.error("invalid Lua register frame"));
    }
    let count = r.word()? as usize;
    let words = r.words(count)?;
    let count = r.word()? as usize;
    let mut constants = Vec::with_capacity(r.count(count, 1)?);
    for _ in 0..count {
        let value = match r.byte()? {
            0 => Const::Nil,
            1 => Const::Bool(r.boolean()?),
            3 => Const::Number(f64::from_bits(r.wide()?)),
            4 => Const::String(
                string(r, size_t)?
                    .ok_or_else(|| r.error("missing string constant"))?
                    .to_vec(),
            ),
            _ => return Err(r.error("invalid Lua 5.1 constant tag")),
        };
        constants.push(PoolValue::Literal(value));
    }
    let count = r.word()? as usize;
    let children = (0..r.count(count, 1)?)
        .map(|_| read_function(r, size_t, depth + 1))
        .collect::<Result<_, _>>()?;
    let lines = r.word()? as usize;
    r.take(r.count(lines, 4)? * 4)?;
    let locals = r.word()? as usize;
    for _ in 0..r.count(locals, 8)? {
        string(r, size_t)?;
        r.take(8)?;
    }
    let names = r.word()? as usize;
    for _ in 0..r.count(names, size_t as usize)? {
        string(r, size_t)?;
    }
    Ok(Function {
        image: Image {
            stack,
            upvalues,
            params,
            variadic: vararg_flags & 2 != 0,
            words,
            constants,
        },
        children,
        vararg_flags,
    })
}

fn lower_tree(program: &mut Vec<Proto>, function: &Function, name: String) -> Result<(), String> {
    let mut lowered = Vec::new();
    let mut pc = 0;
    while pc < function.image.words.len() {
        let (width, mut instructions) =
            lower(function, &name, pc).map_err(|err| format!("Lua 5.1 {name} pc {pc}: {err}"))?;
        if pc == 0 && function.vararg_flags & 4 != 0 {
            // Lua 5.1's implicit arg table is specific to that ABI.
            instructions.insert(0, inst(Op::Native, native_args("lua51", "LEGACY_ARG", &[])));
        }
        lowered.push(Lowered { pc, instructions });
        pc += width;
    }
    let mut proto = assemble(name.clone(), &function.image, lowered)?;
    let children: Vec<String> = (0..function.children.len())
        .map(|id| format!("{name}_{id}"))
        .collect();
    let opaque = proto
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .filter(|inst| inst.op.native())
        .map(ToString::to_string)
        .collect();
    proto.lua51 = Some(Lua51Origin {
        stack: function.image.stack,
        upvalues: function.image.upvalues as u8,
        vararg_flags: function.vararg_flags,
        constants: function
            .image
            .constants
            .iter()
            .map(|value| match value {
                PoolValue::Literal(value) => value.clone(),
                _ => unreachable!("Lua 5.1's constant pool is scalar"),
            })
            .collect(),
        children: children.clone(),
        opaque,
    });
    program.push(proto);
    for (name, function) in children.into_iter().zip(&function.children) {
        lower_tree(program, function, name)?;
    }
    Ok(())
}

fn rk(image: &Image, index: u32, temp: u16, code: &mut Vec<Inst>) -> Result<Operand, String> {
    if index < 256 {
        return image.reg(index);
    }
    let value = image.literal(index - 256)?.unwrap();
    let reg = Operand::Reg(Reg(temp));
    code.push(load(reg.clone(), value));
    Ok(reg)
}

fn lower(function: &Function, proto: &str, pc: usize) -> Result<(usize, Vec<Inst>), String> {
    let image = &function.image;
    let word = image.words[pc];
    let opcode = (word & 63) as usize;
    let name = OPS.get(opcode).ok_or("unknown Lua 5.1 opcode")?;
    let a = (word >> 6) & 255;
    let b = word >> 23;
    let c = (word >> 14) & 511;
    let bx = word >> 14;
    let offset = bx as i32 - 131071;
    let r = |reg| image.reg(reg);
    let mut code = Vec::new();
    let mut width = 1;
    let opaque = || native_args("lua51", name, &[word]);
    if matches!(opcode, 23..=27 | 33)
        && !image.words.get(pc + 1).is_some_and(|next| next & 63 == 22)
    {
        return Err("Lua conditional instruction requires a following JMP".into());
    }
    match opcode {
        0 => code.push(inst(Op::Move, vec![r(a)?, r(b)?])),
        1 => code.push(load(r(a)?, image.literal(bx)?.unwrap())),
        2 => {
            if b > 1 || c > 1 {
                return Err("invalid LOADBOOL fields".into());
            }
            code.push(load(r(a)?, Const::Bool(b != 0)));
            if c != 0 {
                code.push(jump(pc + 2));
            }
        }
        3 => {
            if b < a {
                return Err("invalid LOADNIL range".into());
            }
            for reg in a..=b {
                code.push(load(r(reg)?, Const::Nil));
            }
        }
        12..=15 => {
            let op = [Op::Add, Op::Sub, Op::Mul, Op::Div][opcode - 12];
            let left = rk(image, b, image.stack, &mut code)?;
            let right = rk(image, c, image.stack + 1, &mut code)?;
            code.push(inst(op, vec![r(a)?, left, right]));
        }
        22 => code.push(jump(target(pc, offset)?)),
        23..=25 => {
            if a > 1 {
                return Err("invalid comparison polarity".into());
            }
            let op = [Op::BranchEq, Op::BranchLt, Op::BranchLe][opcode - 23];
            let left = rk(image, b, image.stack, &mut code)?;
            let right = rk(image, c, image.stack + 1, &mut code)?;
            let (yes, no) = if a != 0 {
                (pc + 1, pc + 2)
            } else {
                (pc + 2, pc + 1)
            };
            code.push(branch(op, vec![left, right], yes, no));
        }
        26 => {
            if c > 1 {
                return Err("invalid TEST polarity".into());
            }
            let (yes, no) = if c != 0 {
                (pc + 1, pc + 2)
            } else {
                (pc + 2, pc + 1)
            };
            code.push(branch(Op::Branch, vec![r(a)?], yes, no));
        }
        27 | 33 => code.push(branch(Op::NativeBranch, opaque(), pc + 1, pc + 2)),
        29 => code.push(inst(Op::NativeReturn, opaque())),
        30 if b != 0 => code.push(image.fixed_range(Op::Return, a, b - 1)?),
        30 => code.push(inst(Op::NativeReturn, opaque())),
        31 => code.push(branch(
            Op::NativeBranch,
            opaque(),
            target(pc, offset)?,
            pc + 1,
        )),
        32 => {
            code.push(inst(Op::Native, opaque()));
            code.push(jump(target(pc, offset)?));
        }
        34 if c == 0 => {
            let words = image
                .words
                .get(pc..pc + 2)
                .ok_or("missing extended SETLIST word")?;
            width = words.len();
            code.push(inst(Op::Native, native_args("lua51", name, words)));
        }
        36 => {
            let child = function
                .children
                .get(bx as usize)
                .ok_or("closure prototype is outside the child list")?;
            width += child.image.upvalues;
            let words = image
                .words
                .get(pc..pc + width)
                .ok_or("truncated closure captures")?;
            if words[1..].iter().any(|word| !matches!(word & 63, 0 | 4)) {
                return Err("invalid closure capture instruction".into());
            }
            let mut args = native_args("lua51", name, words);
            args.push(Operand::Const(Const::String(
                format!("{proto}_{bx}").into_bytes(),
            )));
            code.push(inst(Op::Native, args));
        }
        37 if b != 0 => code.push(image.fixed_range(Op::Vararg, a, b - 1)?),
        _ => {
            let mut args = opaque();
            if matches!(opcode, 5 | 7) {
                add_pool_value(&mut args, image, bx)?;
            }
            if matches!(opcode, 9 | 16 | 17) && b >= 256 {
                add_pool_value(&mut args, image, b - 256)?;
            }
            if matches!(opcode, 6 | 9 | 11 | 16 | 17) && c >= 256 {
                add_pool_value(&mut args, image, c - 256)?;
            }
            code.push(inst(Op::Native, args));
        }
    }
    Ok((width, code))
}
