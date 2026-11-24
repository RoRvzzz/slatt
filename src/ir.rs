use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt::{self, Display, Formatter};

#[derive(Clone, Debug, PartialEq)]
pub enum Const {
    Nil,
    Bool(bool),
    Number(f64),
    String(Vec<u8>),
}

impl Const {
    fn truthy(&self) -> bool {
        !matches!(self, Self::Nil | Self::Bool(false))
    }
}

impl Display for Const {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Nil => f.write_str("nil"),
            Self::Bool(value) => write!(f, "{value}"),
            Self::Number(value) if value.is_finite() => write!(f, "{value}"),
            Self::Number(value) => write!(f, "bits(\"{:016x}\")", value.to_bits()),
            Self::String(value) => {
                if let Ok(text) = std::str::from_utf8(value) {
                    return write!(f, "{text:?}");
                }
                f.write_str("\"")?;
                for &byte in value {
                    match byte {
                        b'"' => f.write_str("\\\"")?,
                        b'\\' => f.write_str("\\\\")?,
                        32..=126 => write!(f, "{}", char::from(byte))?,
                        _ => write!(f, "\\x{byte:02x}")?,
                    }
                }
                f.write_str("\"")
            }
        }
    }
}

#[derive(Clone, Copy)]
pub enum Comparison {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

impl Comparison {
    pub fn evaluate(self, left: &Const, right: &Const) -> Option<bool> {
        if matches!(self, Self::Eq | Self::Ne) {
            return Some((left == right) == matches!(self, Self::Eq));
        }
        let (Const::Number(left), Const::Number(right)) = (left, right) else {
            return None;
        };
        Some(match self {
            Self::Lt => left < right,
            Self::Le => left <= right,
            Self::Gt => left > right,
            Self::Ge => left >= right,
            _ => unreachable!(),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Ord, PartialOrd)]
pub struct Reg(pub u16);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Type {
    Reg,
    Const,
    Block,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Operand {
    Reg(Reg),
    Const(Const),
    Block(String),
}

impl Operand {
    pub fn ty(&self) -> Type {
        match self {
            Self::Reg(_) => Type::Reg,
            Self::Const(_) => Type::Const,
            Self::Block(_) => Type::Block,
        }
    }

    fn block(&self) -> &str {
        let Self::Block(name) = self else {
            unreachable!("validated block operand")
        };
        name
    }

    pub fn same_repr(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Const(left), Self::Const(right)) => same_fact(left, right),
            _ => self == other,
        }
    }
}

impl Display for Operand {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Reg(Reg(number)) => write!(f, "r{number}"),
            Self::Const(value) => write!(f, "{value}"),
            Self::Block(name) => write!(f, "@{name}"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    LoadK,
    Move,
    Add,
    Sub,
    Mul,
    Div,
    Xor32,
    Jump,
    Branch,
    BranchEq,
    BranchLt,
    BranchLe,
    Return,
    Native,
    NativeBranch,
    NativeReturn,
}

impl Op {
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "LOADK" => Self::LoadK,
            "MOVE" => Self::Move,
            "ADD" => Self::Add,
            "SUB" => Self::Sub,
            "MUL" => Self::Mul,
            "DIV" => Self::Div,
            "XOR32" => Self::Xor32,
            "JUMP" => Self::Jump,
            "BR_TRUE" => Self::Branch,
            "BR_EQ" => Self::BranchEq,
            "BR_LT" => Self::BranchLt,
            "BR_LE" => Self::BranchLe,
            "RETURN" => Self::Return,
            "NATIVE" => Self::Native,
            "NATIVE_BRANCH" => Self::NativeBranch,
            "NATIVE_RETURN" => Self::NativeReturn,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::LoadK => "LOADK",
            Self::Move => "MOVE",
            Self::Add => "ADD",
            Self::Sub => "SUB",
            Self::Mul => "MUL",
            Self::Div => "DIV",
            Self::Xor32 => "XOR32",
            Self::Jump => "JUMP",
            Self::Branch => "BR_TRUE",
            Self::BranchEq => "BR_EQ",
            Self::BranchLt => "BR_LT",
            Self::BranchLe => "BR_LE",
            Self::Return => "RETURN",
            Self::Native => "NATIVE",
            Self::NativeBranch => "NATIVE_BRANCH",
            Self::NativeReturn => "NATIVE_RETURN",
        }
    }

    fn signature(self) -> &'static [Type] {
        use Type::{Block, Const, Reg};
        match self {
            Self::LoadK => &[Reg, Const],
            Self::Move => &[Reg, Reg],
            Self::Add | Self::Sub | Self::Mul | Self::Div | Self::Xor32 => &[Reg, Reg, Reg],
            Self::Jump => &[Block],
            Self::Branch => &[Reg, Block, Block],
            Self::BranchEq | Self::BranchLt | Self::BranchLe => &[Reg, Reg, Block, Block],
            Self::Return | Self::Native | Self::NativeBranch | Self::NativeReturn => &[],
        }
    }

    pub fn types(self, count: usize) -> Result<Vec<Type>, String> {
        if self == Self::Return {
            return Ok(vec![Type::Reg; count]);
        }
        if self.native() {
            let targets = if self == Self::NativeBranch { 2 } else { 0 };
            if count < targets + 1 {
                return Err(format!(
                    "{} needs an opcode tag and {targets} block operands",
                    self.name()
                ));
            }
            let mut operands = vec![Type::Const; count - targets];
            operands.extend(std::iter::repeat_n(Type::Block, targets));
            return Ok(operands);
        }
        let types = self.signature();
        if types.len() != count {
            return Err(format!(
                "{} expects {} operands, got {count}",
                self.name(),
                types.len()
            ));
        }
        Ok(types.to_vec())
    }

    pub fn terminates(self) -> bool {
        matches!(
            self,
            Self::Jump
                | Self::Branch
                | Self::BranchEq
                | Self::BranchLt
                | Self::BranchLe
                | Self::Return
                | Self::NativeBranch
                | Self::NativeReturn
        )
    }

    fn native(self) -> bool {
        matches!(self, Self::Native | Self::NativeBranch | Self::NativeReturn)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Inst {
    pub op: Op,
    pub args: Vec<Operand>,
}

impl Inst {
    pub fn new(op: Op, args: Vec<Operand>) -> Result<Self, String> {
        for (index, (expected, arg)) in op.types(args.len())?.into_iter().zip(&args).enumerate() {
            if expected != arg.ty() {
                return Err(format!(
                    "{} operand {} expects {expected:?}, got {}",
                    op.name(),
                    index + 1,
                    arg
                ));
            }
        }
        Ok(Self { op, args })
    }

    fn destination(&self) -> Option<Reg> {
        if self.op.terminates() || self.op.native() {
            return None;
        }
        let Operand::Reg(reg) = self.args[0] else {
            unreachable!("validated destination")
        };
        Some(reg)
    }

    fn result(&self, facts: &Facts) -> Option<Const> {
        match self.op {
            Op::LoadK | Op::Move => value_of(&self.args[1], facts).cloned(),
            Op::Add | Op::Sub | Op::Mul | Op::Div | Op::Xor32 => {
                let Const::Number(left) = *value_of(&self.args[1], facts)? else {
                    return None;
                };
                let Const::Number(right) = *value_of(&self.args[2], facts)? else {
                    return None;
                };
                let value = match self.op {
                    Op::Add => left + right,
                    Op::Sub => left - right,
                    Op::Mul => left * right,
                    Op::Div => left / right,
                    Op::Xor32 => f64::from(word(left)? ^ word(right)?),
                    _ => unreachable!(),
                };
                value.is_finite().then_some(Const::Number(value))
            }
            _ => None,
        }
    }

    fn transfer(&self, facts: &mut Facts) {
        if self.op.native() || (self.comparison().is_some() && self.condition(facts).is_none()) {
            facts.clear();
            return;
        }
        let Some(dst) = self.destination() else {
            return;
        };
        if let Some(value) = self.result(facts) {
            facts.insert(dst, value);
        } else if self.op == Op::Move || self.op == Op::Xor32 {
            facts.remove(&dst);
        } else {
            // dynamic arithmetic metamethod can reach captured registers.
            facts.clear();
        }
    }

    fn comparison(&self) -> Option<Comparison> {
        match self.op {
            Op::BranchEq => Some(Comparison::Eq),
            Op::BranchLt => Some(Comparison::Lt),
            Op::BranchLe => Some(Comparison::Le),
            _ => None,
        }
    }

    fn condition(&self, facts: &Facts) -> Option<bool> {
        if self.op == Op::Branch {
            return value_of(&self.args[0], facts).map(Const::truthy);
        }
        self.comparison()?.evaluate(
            value_of(&self.args[0], facts)?,
            value_of(&self.args[1], facts)?,
        )
    }

    fn successors(&self, facts: &Facts) -> &[Operand] {
        let targets = match self.op {
            Op::Branch => &self.args[1..],
            Op::BranchEq | Op::BranchLt | Op::BranchLe => &self.args[2..],
            Op::Jump => return &self.args,
            Op::Return | Op::NativeReturn => return &[],
            Op::NativeBranch => return &self.args[self.args.len() - 2..],
            _ => unreachable!("successors require a terminator"),
        };
        match self.condition(facts) {
            Some(true) => &targets[..1],
            Some(false) => &targets[1..],
            None => targets,
        }
    }
}

fn word(value: f64) -> Option<u32> {
    ((0.0..=f64::from(u32::MAX)).contains(&value) && value.fract() == 0.0).then_some(value as u32)
}

impl Display for Inst {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{}(", self.op.name())?;
        for (index, arg) in self.args.iter().enumerate() {
            if index != 0 {
                f.write_str(", ")?;
            }
            write!(f, "{arg}")?;
        }
        f.write_str(")")
    }
}

pub struct Block {
    pub name: String,
    pub instructions: Vec<Inst>,
}

pub struct Proto {
    pub name: String,
    pub blocks: Vec<Block>,
}

pub type Facts = BTreeMap<Reg, Const>;

pub fn value_of<'a>(operand: &'a Operand, facts: &'a Facts) -> Option<&'a Const> {
    match operand {
        Operand::Const(value) => Some(value),
        Operand::Reg(reg) => facts.get(reg),
        Operand::Block(_) => None,
    }
}

impl Proto {
    fn block_indices(&self) -> Result<BTreeMap<&str, usize>, String> {
        if self.blocks.is_empty() {
            return Err(format!("prototype {} has no entry block", self.name));
        }
        let mut names = BTreeMap::new();
        for (index, block) in self.blocks.iter().enumerate() {
            if names.insert(block.name.as_str(), index).is_some() {
                return Err(format!(
                    "prototype {} has duplicate block @{}",
                    self.name, block.name
                ));
            }
            let Some((last, before)) = block.instructions.split_last() else {
                return Err(format!("@{} is empty", block.name));
            };
            if !last.op.terminates() || before.iter().any(|inst| inst.op.terminates()) {
                return Err(format!(
                    "@{} needs exactly one terminator at the end",
                    block.name
                ));
            }
        }
        for block in &self.blocks {
            for target in block.instructions.last().unwrap().successors(&Facts::new()) {
                if !names.contains_key(target.block()) {
                    return Err(format!("@{} jumps to missing block {target}", block.name));
                }
            }
        }
        Ok(names)
    }

    pub fn verify(&self) -> Result<(), String> {
        self.block_indices().map(|_| ())
    }

    pub fn facts(&self) -> Result<Vec<Vec<Option<Facts>>>, String> {
        let names = self.block_indices()?;
        let mut incoming = vec![None; self.blocks.len()];
        incoming[0] = Some(Facts::new());
        let mut work = VecDeque::from([0]);
        while let Some(index) = work.pop_front() {
            let block = &self.blocks[index];
            let mut facts = incoming[index].clone().unwrap();
            for inst in &block.instructions {
                inst.transfer(&mut facts);
            }
            for successor in block.instructions.last().unwrap().successors(&facts) {
                let dst = names[successor.block()];
                let changed = match &mut incoming[dst] {
                    Some(previous) => {
                        let count = previous.len();
                        previous.retain(|reg, value| {
                            facts
                                .get(reg)
                                .is_some_and(|incoming| same_fact(value, incoming))
                        });
                        count != previous.len()
                    }
                    slot @ None => {
                        *slot = Some(facts.clone());
                        true
                    }
                };
                if changed {
                    work.push_back(dst);
                }
            }
        }
        Ok(self
            .blocks
            .iter()
            .zip(incoming)
            .map(|(block, mut facts)| {
                block
                    .instructions
                    .iter()
                    .map(|inst| {
                        let before = facts.clone();
                        if let Some(facts) = &mut facts {
                            inst.transfer(facts);
                        }
                        before
                    })
                    .collect()
            })
            .collect())
    }

    pub fn fold_constants(&mut self) -> Result<(), String> {
        let facts = self.facts()?;
        for (block, before) in self.blocks.iter_mut().zip(facts) {
            for (inst, facts) in block.instructions.iter_mut().zip(before) {
                if inst.op != Op::LoadK
                    && let (Some(dst), Some(facts)) = (inst.destination(), facts)
                    && let Some(value) = inst.result(&facts)
                {
                    *inst = Inst {
                        op: Op::LoadK,
                        args: vec![Operand::Reg(dst), Operand::Const(value)],
                    };
                }
            }
        }
        Ok(())
    }

    pub fn prune_unreachable(&mut self) -> Result<(), String> {
        let indices = self.block_indices()?;
        let mut reached = vec![false; self.blocks.len()];
        let mut work = vec![0];
        let unknown = Facts::new();
        while let Some(index) = work.pop() {
            if std::mem::replace(&mut reached[index], true) {
                continue;
            }
            let tail = self.blocks[index].instructions.last().unwrap();
            work.extend(
                tail.successors(&unknown)
                    .iter()
                    .map(|target| indices[target.block()]),
            );
        }
        let mut reached = reached.into_iter();
        self.blocks.retain(|_| reached.next().unwrap());
        Ok(())
    }

    pub fn dce(&mut self) -> Result<(), String> {
        self.verify()?;
        // a native closure can make later writes visible through an escaped register cell.
        if self.blocks.iter().flat_map(|block| &block.instructions).any(|inst| {
            inst.op.native() && matches!(&inst.args[0], Operand::Const(Const::String(name))
                if name.ends_with(b".CLOSURE") || name.ends_with(b".NEWCLOSURE") || name.ends_with(b".DUPCLOSURE"))
        }) { return Ok(()); }
        let registers: BTreeSet<_> = self
            .blocks
            .iter()
            .flat_map(|block| &block.instructions)
            .flat_map(|inst| &inst.args)
            .filter_map(operand_register)
            .collect();
        loop {
            let indices = self.block_indices()?;
            let facts = self.facts()?;
            let unknown = Facts::new();
            let mut live_in = vec![BTreeSet::new(); self.blocks.len()];
            let mut keep = self
                .blocks
                .iter()
                .map(|block| vec![true; block.instructions.len()])
                .collect::<Vec<_>>();
            loop {
                let mut changed = false;
                for (index, block) in self.blocks.iter().enumerate().rev() {
                    let mut live: BTreeSet<_> = block
                        .instructions
                        .last()
                        .unwrap()
                        .successors(&unknown)
                        .iter()
                        .flat_map(|target| live_in[indices[target.block()]].iter().copied())
                        .collect();
                    for (pc, inst) in block.instructions.iter().enumerate().rev() {
                        let dst = inst.destination();
                        let pure = matches!(inst.op, Op::LoadK | Op::Move)
                            || facts[index][pc]
                                .as_ref()
                                .is_some_and(|before| inst.result(before).is_some());
                        keep[index][pc] = !(pure && dst.is_some_and(|reg| !live.contains(&reg)));
                        if !keep[index][pc] {
                            continue;
                        }
                        if let Some(dst) = dst {
                            live.remove(&dst);
                        }
                        if inst.op.native() {
                            live.extend(&registers);
                        } else {
                            live.extend(
                                inst.args[usize::from(dst.is_some())..]
                                    .iter()
                                    .filter_map(operand_register),
                            );
                        }
                    }
                    changed |= live_in[index] != live;
                    live_in[index] = live;
                }
                if !changed {
                    break;
                }
            }
            let mut removed = false;
            for (block, keep) in self.blocks.iter_mut().zip(keep) {
                let mut keep = keep.into_iter();
                let len = block.instructions.len();
                block.instructions.retain(|_| keep.next().unwrap());
                removed |= len != block.instructions.len();
            }
            if !removed {
                return Ok(());
            }
        }
    }
}

fn operand_register(operand: &Operand) -> Option<Reg> {
    match operand {
        Operand::Reg(reg) => Some(*reg),
        _ => None,
    }
}

// distinguish signed zeros at joins because subsequent division can observe the sign.
fn same_fact(left: &Const, right: &Const) -> bool {
    match (left, right) {
        (Const::Number(left), Const::Number(right)) => left.to_bits() == right.to_bits(),
        _ => left == right,
    }
}

impl Display for Proto {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        writeln!(f, "proto {} {{", self.name)?;
        for block in &self.blocks {
            writeln!(f, "  @{}:", block.name)?;
            for inst in &block.instructions {
                writeln!(f, "    {inst};")?;
            }
        }
        writeln!(f, "}}")
    }
}
