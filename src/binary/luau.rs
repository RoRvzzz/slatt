use super::*;

struct Function {
    image: Image,
    children: Vec<usize>,
}

// Opcode ordering and auxiliary words for the Luau formats decoded here.
const OPS: &[&str] = &[
    "NOP",
    "BREAK",
    "LOADNIL",
    "LOADB",
    "LOADN",
    "LOADK",
    "MOVE",
    "GETGLOBAL",
    "SETGLOBAL",
    "GETUPVAL",
    "SETUPVAL",
    "CLOSEUPVALS",
    "GETIMPORT",
    "GETTABLE",
    "SETTABLE",
    "GETTABLEKS",
    "SETTABLEKS",
    "GETTABLEN",
    "SETTABLEN",
    "NEWCLOSURE",
    "NAMECALL",
    "CALL",
    "RETURN",
    "JUMP",
    "JUMPBACK",
    "JUMPIF",
    "JUMPIFNOT",
    "JUMPIFEQ",
    "JUMPIFLE",
    "JUMPIFLT",
    "JUMPIFNOTEQ",
    "JUMPIFNOTLE",
    "JUMPIFNOTLT",
    "ADD",
    "SUB",
    "MUL",
    "DIV",
    "MOD",
    "POW",
    "ADDK",
    "SUBK",
    "MULK",
    "DIVK",
    "MODK",
    "POWK",
    "AND",
    "OR",
    "ANDK",
    "ORK",
    "CONCAT",
    "NOT",
    "MINUS",
    "LENGTH",
    "NEWTABLE",
    "DUPTABLE",
    "SETLIST",
    "FORNPREP",
    "FORNLOOP",
    "FORGLOOP",
    "FORGPREP_INEXT",
    "FASTCALL3",
    "FORGPREP_NEXT",
    "NATIVECALL",
    "GETVARARGS",
    "DUPCLOSURE",
    "PREPVARARGS",
    "LOADKX",
    "JUMPX",
    "FASTCALL",
    "COVERAGE",
    "CAPTURE",
    "SUBRK",
    "DIVRK",
    "FASTCALL1",
    "FASTCALL2",
    "FASTCALL2K",
    "FORGPREP",
    "JUMPXEQKNIL",
    "JUMPXEQKB",
    "JUMPXEQKN",
    "JUMPXEQKS",
    "IDIV",
    "IDIVK",
    "GETUDATAKS",
    "SETUDATAKS",
    "NAMECALLUDATA",
    "NEWCLASSMEMBER",
    "CALLFB",
    "CMPPROTO",
    "FASTPCALL",
];

pub(super) fn import(bytes: &[u8]) -> Result<Vec<Proto>, String> {
    let mut r = Reader::new(bytes);
    let version = r.byte()?;
    if version == 0 {
        return Err("Luau compiler error record, not bytecode".into());
    }
    if !matches!(version, 6..=14) {
        return Err(format!(
            "unsupported Luau bytecode version {version} (supported: 6–14)"
        ));
    }
    let types = r.byte()?;
    if !(1..=3).contains(&types) {
        return Err(r.error("unsupported Luau type encoding"));
    }
    let count = r.var32()?;
    let mut strings = Vec::with_capacity(r.count(count, 1)?);
    for _ in 0..count {
        let size = r.var32()?;
        strings.push(r.take(size)?);
    }
    if types == 3 {
        while r.byte()? != 0 {
            string(&mut r, &strings)?;
        }
    }
    let count = r.var32()?;
    let mut functions = Vec::with_capacity(r.count(count, 1)?);
    for id in 0..count {
        let function = if version >= 12 {
            let size = r.var32()?;
            let mut region = Reader::new(r.take(size)?);
            read_function(&mut region, &strings, id, version)
        } else {
            read_function(&mut r, &strings, id, version)
        };
        functions.push(function.map_err(|err| format!("Luau prototype {id}: {err}"))?);
    }
    let main = r.var32()?;
    if main >= functions.len() {
        return Err(r.error("main prototype is outside the prototype list"));
    }
    r.end()?;
    let mut program = Vec::new();
    for id in std::iter::once(main).chain((0..count).filter(|id| *id != main)) {
        let mut lowered = Vec::new();
        let function = &functions[id];
        let name = proto_name(id, main);
        let mut pc = 0;
        while pc < function.image.words.len() {
            let (width, instructions) = lower(&functions, function, main, version, pc)
                .map_err(|err| format!("Luau {name} pc {pc}: {err}"))?;
            lowered.push(Lowered { pc, instructions });
            pc += width;
        }
        if function.image.variadic
            && lowered
                .iter()
                .skip(1)
                .flat_map(|site| &site.instructions)
                .flat_map(|inst| &inst.args)
                .any(|target| target == &label(0))
        {
            return Err(format!("Luau {name} jumps into the vararg prologue"));
        }
        program.push(assemble(name, &function.image, lowered)?);
    }
    Ok(program)
}

fn string<'a>(r: &mut Reader<'_>, strings: &[&'a [u8]]) -> Result<Option<&'a [u8]>, String> {
    match r.var32()? {
        0 => Ok(None),
        id => strings
            .get(id - 1)
            .copied()
            .map(Some)
            .ok_or_else(|| r.error("invalid string id")),
    }
}

fn proto_id(r: &mut Reader<'_>, id: usize) -> Result<usize, String> {
    let child = r.var32()?;
    if child >= id {
        return Err(r.error("invalid forward or missing child prototype"));
    }
    Ok(child)
}

fn read_function(
    r: &mut Reader<'_>,
    strings: &[&[u8]],
    id: usize,
    version: u8,
) -> Result<Function, String> {
    let stack = u16::from(r.byte()?);
    let params = r.byte()?;
    let upvalues = usize::from(r.byte()?);
    let variadic = r.boolean()?;
    let flags = r.byte()?;
    if stack == 0 || u16::from(params) > stack {
        return Err(r.error("invalid Luau register frame"));
    }
    let types = r.var32()?;
    r.take(types)?;
    let count = r.var32()?;
    let words = r.words(count)?;
    if variadic && words.first().copied() != Some(65 | u32::from(params) << 8) {
        return Err(r.error("variadic prototype lacks its PREPVARARGS prologue"));
    }
    let count = r.var32()?;
    let mut constants = Vec::with_capacity(r.count(count, 1)?);
    for _ in 0..count {
        let value = match r.byte()? {
            0 => PoolValue::Literal(Const::Nil),
            1 => PoolValue::Literal(Const::Bool(r.boolean()?)),
            2 => PoolValue::Literal(Const::Number(f64::from_bits(r.wide()?))),
            3 => PoolValue::Literal(Const::String(
                string(r, strings)?
                    .ok_or_else(|| r.error("missing string constant"))?
                    .to_vec(),
            )),
            4 => PoolValue::Native(format!("import 0x{:08x}", r.word()?)),
            tag @ (5 | 8) if tag == 5 || version >= 7 => {
                let keys = r.var32()?;
                let mut fields = Vec::with_capacity(r.count(keys, if tag == 8 { 5 } else { 1 })?);
                for _ in 0..keys {
                    let key = r.var32()?;
                    if key >= count {
                        return Err(r.error("invalid table template key"));
                    }
                    if tag == 8 {
                        let value = r.word()? as i32;
                        if value >= 0 && value as usize >= count {
                            return Err(r.error("invalid table template value"));
                        }
                        fields.push(format!("{key}:{value}"));
                    } else {
                        fields.push(key.to_string());
                    }
                }
                PoolValue::Native(format!("table [{}]", fields.join(", ")))
            }
            6 => PoolValue::Closure(proto_id(r, id)?),
            7 => {
                let mut values = [0; 4];
                for value in &mut values {
                    *value = r.word()?;
                }
                PoolValue::Native(format!(
                    "vector [{:08x}, {:08x}, {:08x}, {:08x}]",
                    values[0], values[1], values[2], values[3]
                ))
            }
            9 if version >= 8 => {
                let negative = r.boolean()?;
                let raw = r.var(64)?;
                if (!negative && raw > i64::MAX as u64) || (negative && raw > 1u64 << 63) {
                    return Err(r.error("invalid Luau integer magnitude"));
                }
                let value = if negative { raw.wrapping_neg() } else { raw } as i64;
                PoolValue::Native(format!("integer {value}"))
            }
            10 if version >= 10 => {
                let name = r.var32()?;
                let properties = r.var32()?;
                let methods = r.var32()?;
                let members = properties
                    .checked_add(methods)
                    .ok_or_else(|| r.error("invalid class member count"))?;
                if name >= count {
                    return Err(r.error("invalid class name"));
                }
                let mut ids = Vec::with_capacity(r.count(members, 1)?);
                for _ in 0..members {
                    let member = r.var32()?;
                    if member >= count {
                        return Err(r.error("invalid class member name"));
                    }
                    ids.push(member.to_string());
                }
                PoolValue::Native(format!(
                    "class {name} [{properties}/{methods}: {}]",
                    ids.join(", ")
                ))
            }
            11 if version >= 13 => {
                let mut values = [0; 4];
                for value in &mut values {
                    *value = r.wide()?;
                }
                PoolValue::Native(format!(
                    "vector64 [{:016x}, {:016x}, {:016x}, {:016x}]",
                    values[0], values[1], values[2], values[3]
                ))
            }
            _ => return Err(r.error("unsupported or invalid Luau constant tag")),
        };
        constants.push(value);
    }
    let count = r.var32()?;
    let children = (0..r.count(count, 1)?)
        .map(|_| proto_id(r, id))
        .collect::<Result<_, _>>()?;
    r.var32()?; // defined line
    string(r, strings)?; // debug name
    if r.boolean()? {
        let gap = r.byte()?;
        let last = words
            .len()
            .checked_sub(1)
            .ok_or_else(|| r.error("line info for an empty prototype"))?;
        let intervals = last
            .checked_shr(gap.into())
            .ok_or_else(|| r.error("invalid line info gap"))?
            + 1;
        r.take(words.len())?;
        r.take(r.count(intervals, 4)? * 4)?;
    }
    if r.boolean()? {
        let count = r.var32()?;
        for _ in 0..r.count(count, 4)? {
            string(r, strings)?;
            r.var32()?;
            r.var32()?;
            r.byte()?;
        }
        let count = r.var32()?;
        for _ in 0..r.count(count, 1)? {
            string(r, strings)?;
        }
    }
    if version >= 11 {
        let count = r.var32()?;
        for _ in 0..r.count(count, 2)? {
            if r.byte()? != 0 || r.var32()? >= words.len() {
                return Err(r.error("invalid Luau call feedback slot"));
            }
        }
    }
    if version >= 12 && flags & (1 << 3) != 0 {
        r.var(64)?;
    }
    Ok(Function {
        image: Image {
            stack,
            upvalues,
            params,
            variadic,
            words,
            constants,
        },
        children,
    })
}

fn proto_name(id: usize, main: usize) -> String {
    if id == main {
        "main".into()
    } else {
        format!("proto{id}")
    }
}

fn lower(
    functions: &[Function],
    function: &Function,
    main: usize,
    version: u8,
    pc: usize,
) -> Result<(usize, Vec<Inst>), String> {
    let image = &function.image;
    let word = image.words[pc];
    let opcode = (word & 255) as usize;
    let name = OPS.get(opcode).ok_or("unknown Luau opcode")?;
    let minimum_version = match opcode {
        83..=85 => 9,
        86 => 10,
        87 | 88 => 11,
        89 => 14,
        _ => 6,
    };
    if version < minimum_version {
        return Err(format!(
            "{name} requires Luau bytecode version {minimum_version}"
        ));
    }
    let a = (word >> 8) & 255;
    let b = (word >> 16) & 255;
    let c = word >> 24;
    let d = (word as i32) >> 16;
    let unsigned_d = || u32::try_from(d).map_err(|_| "negative constant or child index");
    let r = |reg| image.reg(reg);
    let dest = || target(pc, d);
    let mut width = if matches!(opcode, 7 | 8 | 12 | 15 | 16 | 20 | 27..=32 | 53 | 55 | 58 | 60 | 66 | 74 | 75 | 77..=80 | 83..=88)
    {
        2
    } else {
        1
    };
    let words = image
        .words
        .get(pc..pc + width)
        .ok_or("missing Luau auxiliary word")?;
    let aux = words.get(1).copied().unwrap_or(0);
    let next = pc + width;
    let opaque = || native_args("luau", name, words);
    let mut code = Vec::new();
    match opcode {
        0 => {}
        1 | 62 => return Err(format!("runtime-only opcode {name}")),
        2 => code.push(load(r(a)?, Const::Nil)),
        3 => {
            if b > 1 {
                return Err("invalid LOADB boolean".into());
            }
            code.push(load(r(a)?, Const::Bool(b != 0)));
            if c != 0 {
                code.push(jump(pc + 1 + c as usize));
            }
        }
        4 => code.push(load(r(a)?, number(d))),
        5 | 66 => {
            let index = if opcode == 5 { unsigned_d()? } else { aux };
            if let Some(value) = image.literal(index)? {
                code.push(load(r(a)?, value));
            } else {
                let mut args = opaque();
                add_pool_value(&mut args, image, index)?;
                code.push(inst(Op::Native, args));
            }
        }
        6 => code.push(inst(Op::Move, vec![r(a)?, r(b)?])),
        19 | 64 => {
            let index = unsigned_d()?;
            let id = if opcode == 19 {
                *function
                    .children
                    .get(index as usize)
                    .ok_or("invalid NEWCLOSURE child index")?
            } else {
                let PoolValue::Closure(id) = image.constant(index)? else {
                    return Err("DUPCLOSURE constant is not a closure".into());
                };
                *id
            };
            width += functions[id].image.upvalues;
            let words = image
                .words
                .get(pc..pc + width)
                .ok_or("truncated closure captures")?;
            if words[1..].iter().any(|word| word & 255 != 70) {
                return Err("invalid Luau closure capture".into());
            }
            let mut args = native_args("luau", name, words);
            args.push(Operand::Const(Const::String(
                proto_name(id, main).into_bytes(),
            )));
            code.push(inst(Op::Native, args));
        }
        22 if b != 0 => code.push(image.fixed_range(Op::Return, a, b - 1)?),
        22 => code.push(inst(Op::NativeReturn, opaque())),
        23 | 24 => code.push(jump(dest()?)),
        25 | 26 => {
            let target = dest()?;
            let (yes, no) = if opcode == 25 {
                (target, next)
            } else {
                (next, target)
            };
            code.push(branch(Op::Branch, vec![r(a)?], yes, no));
        }
        27..=32 => {
            let op = [Op::BranchEq, Op::BranchLe, Op::BranchLt][(opcode - 27) % 3];
            let target = dest()?;
            let (yes, no) = if opcode <= 29 {
                (target, next)
            } else {
                (next, target)
            };
            code.push(branch(op, vec![r(a)?, r(aux)?], yes, no));
        }
        33..=36 => {
            let op = [Op::Add, Op::Sub, Op::Mul, Op::Div][opcode - 33];
            code.push(inst(op, vec![r(a)?, r(b)?, r(c)?]));
        }
        39..=42 | 71 | 72 => {
            let reverse = opcode >= 71;
            let index = if reverse { b } else { c };
            if let Some(value) = image.literal(index)? {
                let op = match opcode {
                    39 => Op::Add,
                    40 | 71 => Op::Sub,
                    41 => Op::Mul,
                    _ => Op::Div,
                };
                let temp = Operand::Reg(Reg(image.stack));
                let other = r(if reverse { c } else { b })?;
                code.push(load(temp.clone(), value));
                let (left, right) = if reverse {
                    (temp, other)
                } else {
                    (other, temp)
                };
                code.push(inst(op, vec![r(a)?, left, right]));
            } else {
                let mut args = opaque();
                add_pool_value(&mut args, image, index)?;
                code.push(inst(Op::Native, args));
            }
        }
        56..=58 | 88 => code.push(branch(Op::NativeBranch, opaque(), dest()?, next)),
        59 | 61 | 76 => {
            code.push(inst(Op::Native, opaque()));
            code.push(jump(dest()?));
        }
        60 | 68 | 73..=75 | 89 => {
            let mut args = opaque();
            if opcode == 75 {
                add_pool_value(&mut args, image, aux)?;
            }
            let call = pc + c as usize + 1;
            let call_width = match image.words.get(call).map(|word| word & 255) {
                Some(21) => 1,
                Some(87) if opcode != 89 => 2,
                _ => return Err("fastcall target is not a compatible CALL".into()),
            };
            code.push(branch(Op::NativeBranch, args, call + call_width, next));
        }
        63 if b != 0 => code.push(image.fixed_range(Op::Vararg, a, b - 1)?),
        65 if pc == 0 && image.variadic => {}
        65 => return Err("PREPVARARGS outside a variadic prototype prologue".into()),
        67 => code.push(jump(target(pc, (word as i32) >> 8)?)),
        70 => return Err("CAPTURE outside closure instruction".into()),
        77..=80 => {
            let value = match opcode {
                77 => Const::Nil,
                78 => Const::Bool(aux & 1 != 0),
                _ => image
                    .literal(aux & 0x00ff_ffff)?
                    .ok_or("comparison requires a scalar constant")?,
            };
            if (opcode == 79 && !matches!(value, Const::Number(_)))
                || (opcode == 80 && !matches!(value, Const::String(_)))
            {
                return Err("comparison constant has the wrong type".into());
            }
            let temp = Operand::Reg(Reg(image.stack));
            code.push(load(temp.clone(), value));
            let target = dest()?;
            let (yes, no) = if aux >> 31 == 0 {
                (target, next)
            } else {
                (next, target)
            };
            code.push(branch(Op::BranchEq, vec![r(a)?, temp], yes, no));
        }
        _ => {
            let mut args = opaque();
            let constant = match opcode {
                7 | 8 | 15 | 16 | 20 => Some(aux),
                12 | 54 => Some(unsigned_d()?),
                43 | 44 | 47 | 48 | 82 => Some(c),
                83..=85 => Some(aux & 0xffff),
                _ => None,
            };
            if let Some(index) = constant {
                add_pool_value(&mut args, image, index)?;
            }
            code.push(inst(Op::Native, args));
        }
    }
    Ok((width, code))
}
