use crate::ir::{Block, Const, Inst, Op, Operand, Proto, Reg, Type};
use rhai::{Array, Blob, Dynamic, Engine, EvalAltResult, ImmutableString, Map};
use std::collections::BTreeSet;
use std::rc::Rc;

#[derive(Clone)]
pub struct Lifted(Rc<Proto>);

impl Lifted {
    pub fn into_proto(self) -> Proto {
        Rc::unwrap_or_clone(self.0)
    }
}

pub fn program(values: Array) -> Result<Vec<Proto>, String> {
    if values.is_empty() {
        return Err("PFIR program requires a root prototype".into());
    }
    values
        .into_iter()
        .map(|value| {
            value
                .try_cast::<Lifted>()
                .map(Lifted::into_proto)
                .ok_or_else(|| "PFIR program requires prototype values".into())
        })
        .collect()
}

fn identifier(name: &str) -> Result<(), Box<EvalAltResult>> {
    let mut bytes = name.bytes();
    if bytes
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == b'_')
        && bytes.all(|c| c.is_ascii_alphanumeric() || c == b'_')
    {
        Ok(())
    } else {
        Err(format!("invalid PFIR identifier {name:?}").into())
    }
}

fn operand(value: Dynamic) -> Result<Operand, Box<EvalAltResult>> {
    if value.is::<Operand>() {
        return Ok(value.cast::<Operand>());
    }
    let constant = if value.is_unit() {
        Const::Nil
    } else if let Ok(boolean) = value.as_bool() {
        Const::Bool(boolean)
    } else if let Ok(integer) = value.as_int() {
        if !(-(1i64 << 53)..=(1i64 << 53)).contains(&integer) {
            return Err("PFIR integer is outside the exact double-precision range".into());
        }
        Const::Number(integer as f64)
    } else if let Ok(number) = value.as_float() {
        Const::Number(number)
    } else if value.is::<Blob>() {
        Const::String(value.cast())
    } else if let Some(text) = value.try_cast::<ImmutableString>() {
        Const::String(text.as_bytes().to_vec())
    } else {
        return Err(
            "invalid PFIR operand; expected a register, block reference, or scalar constant".into(),
        );
    };
    Ok(Operand::Const(constant))
}

fn instruction(name: &str, args: Array) -> Result<Inst, Box<EvalAltResult>> {
    let op = Op::parse(name).ok_or_else(|| format!("unknown PFIR opcode {name}"))?;
    let args = expand_register_windows(args)?;
    let instruction = Inst::new(op, args.into_iter().map(operand).collect::<Result<_, _>>()?)?;
    Ok(instruction)
}

fn register_operand(value: i64) -> Result<Operand, Box<EvalAltResult>> {
    Ok(Operand::Reg(Reg(
        u16::try_from(value).map_err(|_| "PFIR register must be 0–65535")?
    )))
}

const REGISTER_COUNT: i64 = u16::MAX as i64 + 1;

#[derive(Clone)]
struct RegisterWindow {
    first: i64,
    end: i64,
}

fn register_window(first: i64, count: i64) -> Result<RegisterWindow, Box<EvalAltResult>> {
    let end = first
        .checked_add(count)
        .filter(|&end| first >= 0 && count >= 0 && end <= REGISTER_COUNT)
        .ok_or("PFIR register window exceeds r0–r65535 or has a negative bound")?;
    Ok(RegisterWindow { first, end })
}

fn expand_register_windows(args: Array) -> Result<Array, Box<EvalAltResult>> {
    if !args.iter().any(Dynamic::is::<RegisterWindow>) {
        return Ok(args);
    }
    let mut output = Vec::with_capacity(args.len());
    for value in args {
        if value.is::<RegisterWindow>() {
            let window = value.cast::<RegisterWindow>();
            if output.len() + (window.end - window.first) as usize > REGISTER_COUNT as usize {
                return Err("PFIR register window expansion exceeds 65,536 operands".into());
            }
            output.extend(
                (window.first..window.end)
                    .map(|index| Dynamic::from(Operand::Reg(Reg(index as u16)))),
            );
        } else {
            if output.len() == REGISTER_COUNT as usize {
                return Err("PFIR register window expansion exceeds 65,536 operands".into());
            }
            output.push(value);
        }
    }
    Ok(output)
}

fn pc_operand(value: i64) -> Result<Operand, Box<EvalAltResult>> {
    let pc = usize::try_from(value).map_err(|_| "PFIR PC must be a nonnegative index")?;
    Ok(Operand::Block(pc_label(pc)))
}

fn assemble(name: &str, args: Array) -> Result<Inst, Box<EvalAltResult>> {
    let op = Op::parse(name).ok_or_else(|| format!("unknown PFIR opcode {name}"))?;
    let args = expand_register_windows(args)?;
    let args = op
        .types(args.len())?
        .into_iter()
        .zip(args)
        .map(|(ty, arg)| match (ty, arg.as_int()) {
            (Type::Reg | Type::Capture, Ok(reg)) => register_operand(reg),
            (Type::Block, Ok(pc)) => pc_operand(pc),
            _ => operand(arg),
        })
        .collect::<Result<_, _>>()?;
    Ok(Inst::new(op, args)?)
}

fn block(name: &str, instructions: Array) -> Result<Block, Box<EvalAltResult>> {
    identifier(name)?;
    let instructions = instructions
        .into_iter()
        .map(|value| {
            value
                .try_cast::<Inst>()
                .ok_or("PFIR block needs instruction values")
        })
        .collect::<Result<_, _>>()?;
    Ok(Block {
        name: name.to_owned(),
        instructions,
    })
}

fn prototype(
    name: &str,
    params: i64,
    variadic: bool,
    blocks: Array,
) -> Result<Lifted, Box<EvalAltResult>> {
    let blocks = blocks
        .into_iter()
        .map(|value| {
            value
                .try_cast::<Block>()
                .ok_or("PFIR prototype needs block values")
        })
        .collect::<Result<_, _>>()?;
    lift(name, params, variadic, blocks)
}

fn lift(
    name: &str,
    params: i64,
    variadic: bool,
    blocks: Vec<Block>,
) -> Result<Lifted, Box<EvalAltResult>> {
    identifier(name)?;
    let params = u8::try_from(params).map_err(|_| "PFIR parameter count must be 0–255")?;
    let proto = Proto {
        name: name.to_owned(),
        params,
        variadic,
        blocks,
        lua51: None,
    };
    proto.verify()?;
    Ok(Lifted(Rc::new(proto)))
}

fn linear(
    name: &str,
    params: i64,
    variadic: bool,
    values: Array,
) -> Result<Lifted, Box<EvalAltResult>> {
    let count = values.len();
    let bundles = values
        .into_iter()
        .map(|value| {
            if value.is::<Inst>() {
                return Ok(vec![value.cast::<Inst>()]);
            }
            let bundle = value
                .try_cast::<Array>()
                .ok_or("PFIR linear input needs instructions or instruction bundles")?;
            let bundle = bundle
                .into_iter()
                .map(|value| {
                    value
                        .try_cast::<Inst>()
                        .ok_or("PFIR bundle needs instructions")
                })
                .collect::<Result<Vec<_>, _>>()?;
            let Some((_, before)) = bundle.split_last() else {
                return Err("PFIR instruction bundle is empty");
            };
            if before.iter().any(|inst| inst.op.terminates()) {
                return Err("PFIR instruction bundle has an early terminator");
            }
            Ok(bundle)
        })
        .collect::<Result<Vec<_>, _>>()?;
    if !bundles
        .last()
        .is_some_and(|bundle| bundle.last().unwrap().op.terminates())
    {
        return Err("PFIR linear input must end in a terminator".into());
    }

    let mut starts = BTreeSet::from([0, count]);
    for (pc, bundle) in bundles.iter().enumerate() {
        let inst = bundle.last().unwrap();
        if inst.op.terminates() {
            starts.insert(pc + 1);
        }
        for target in inst.args.iter().filter_map(|arg| match arg {
            Operand::Block(target) => Some(target.as_str()),
            _ => None,
        }) {
            let destination = target
                .strip_prefix("pc")
                .and_then(|text| text.parse::<usize>().ok())
                .filter(|&target_pc| target_pc < count && target == pc_label(target_pc))
                .ok_or_else(|| format!("PFIR instruction {pc} has invalid PC target @{target}"))?;
            starts.insert(destination);
        }
    }
    let mut remaining = bundles.into_iter();
    let mut blocks = Vec::with_capacity(starts.len() - 1);
    for (first, next) in starts.iter().copied().zip(starts.iter().copied().skip(1)) {
        let mut instructions: Vec<_> = remaining.by_ref().take(next - first).flatten().collect();
        if !instructions.last().unwrap().op.terminates() {
            instructions.push(Inst::new(Op::Jump, vec![Operand::Block(pc_label(next))])?);
        }
        blocks.push(Block {
            name: pc_label(first),
            instructions,
        });
    }
    lift(name, params, variadic, blocks)
}

fn pc_label(pc: usize) -> String {
    format!("pc{pc}")
}

fn jump_table(register: i64, count: i64) -> Result<Inst, Box<EvalAltResult>> {
    if !(1..=65_536).contains(&count) {
        return Err("PFIR jump table expects 1–65,536 physical PCs".into());
    }
    let mut args = Vec::with_capacity(count as usize + 1);
    args.push(register_operand(register)?);
    args.extend((0..count as usize).map(|pc| Operand::Block(pc_label(pc))));
    Ok(Inst::new(Op::JumpTable, args)?)
}

fn cfg(lifted: &mut Lifted) -> Result<Array, Box<EvalAltResult>> {
    let proto = &lifted.0;
    let edges = proto.control_flow()?;
    let mut predecessors = vec![Vec::new(); proto.blocks.len()];
    for (from, successors) in edges.iter().enumerate() {
        for &to in successors {
            if predecessors[to].last() != Some(&from) {
                predecessors[to].push(from);
            }
        }
    }
    let mut reachable = vec![false; proto.blocks.len()];
    let mut stack = vec![0usize];
    while let Some(id) = stack.pop() {
        if !std::mem::replace(&mut reachable[id], true) {
            stack.extend(&edges[id]);
        }
    }
    Ok(proto
        .blocks
        .iter()
        .zip(edges)
        .zip(predecessors)
        .zip(reachable)
        .map(|(((block, edges), previous), reached)| {
            let targets: Array = edges
                .into_iter()
                .map(|id| proto.blocks[id].name.clone().into())
                .collect();
            let previous: Array = previous
                .into_iter()
                .map(|id| proto.blocks[id].name.clone().into())
                .collect();
            Dynamic::from_map(Map::from([
                ("name".into(), block.name.clone().into()),
                ("predecessors".into(), Dynamic::from_array(previous)),
                ("successors".into(), Dynamic::from_array(targets)),
                ("reachable".into(), reached.into()),
                (
                    "instructions".into(),
                    (block.instructions.len() as i64).into(),
                ),
            ]))
        })
        .collect())
}

pub fn register(engine: &mut Engine) {
    engine.register_type_with_name::<Operand>("PfOperand");
    engine.register_type_with_name::<RegisterWindow>("PfRegisterWindow");
    engine.register_type_with_name::<Inst>("PfInstruction");
    engine.register_type_with_name::<Block>("PfBlock");
    engine.register_type_with_name::<Lifted>("PfPrototype");
    engine.register_fn("pfir_reg", register_operand);
    engine.register_fn("pfir_regs", register_window);
    engine.register_fn(
        "pfir_upvalue",
        |slot: i64| -> Result<Operand, Box<EvalAltResult>> {
            if !(0..=254).contains(&slot) {
                return Err("PFIR upvalue slot must be 0–254".into());
            }
            Ok(Operand::Const(Const::Number(slot as f64)))
        },
    );
    engine.register_fn(
        "pfir_target",
        |name: &str| -> Result<Operand, Box<EvalAltResult>> {
            identifier(name)?;
            Ok(Operand::Block(name.to_owned()))
        },
    );
    engine.register_fn("pfir_inst", instruction);
    engine.register_fn("pfir_asm", assemble);
    engine.register_fn("pfir_block", block);
    engine.register_fn("pfir_proto", prototype);
    engine.register_fn("pfir_pc", pc_operand);
    engine.register_fn("pfir_jump_table", jump_table);
    engine.register_fn("pfir_linear", linear);
    engine.register_fn("cfg", cfg);
    engine.register_fn("pfir_text", |program: &mut Lifted| program.0.to_string());
    engine.register_fn("pfir_text", |instruction: &mut Inst| {
        instruction.to_string()
    });
    engine.register_fn(
        "emit_lua",
        |program: &mut Lifted| -> Result<String, Box<EvalAltResult>> {
            Ok(crate::lua_emit::emit(&program.0)?)
        },
    );
    engine.register_fn(
        "emit_lua",
        |values: Array| -> Result<String, Box<EvalAltResult>> {
            Ok(crate::lua_emit::emit_program(&program(values)?)?)
        },
    );
}
