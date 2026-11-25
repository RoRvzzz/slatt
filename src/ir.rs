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
    Capture,
}

impl Type {
    pub(crate) fn accepts(self, actual: Self) -> bool {
        self == actual || (self == Self::Capture && matches!(actual, Self::Reg | Self::Const))
    }
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
    Nop,
    LoadK,
    GetGlobal,
    Call,
    CallOpen,
    CallFromTop,
    TailCall,
    Closure,
    CloseUpvalues,
    GetUpvalue,
    SetUpvalue,
    NewTable,
    GetTable,
    SetTable,
    SelfCall,
    SetList,
    Not,
    Len,
    Pow,
    Concat,
    Eq,
    Ne,
    Lt,
    Move,
    Add,
    Sub,
    Mul,
    Div,
    Xor32,
    Jump,
    JumpTable,
    ForNext,
    Branch,
    BranchEq,
    BranchLt,
    BranchLe,
    Vararg,
    Return,
    ReturnTop,
    Native,
    NativeBranch,
    NativeReturn,
}

impl Op {
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "NOP" => Self::Nop,
            "LOADK" => Self::LoadK,
            "GETGLOBAL" => Self::GetGlobal,
            "CALL" => Self::Call,
            "CALL_OPEN" => Self::CallOpen,
            "CALL_FROM_TOP" => Self::CallFromTop,
            "TAILCALL" => Self::TailCall,
            "CLOSURE" => Self::Closure,
            "CLOSEUPVALS" => Self::CloseUpvalues,
            "GETUPVAL" => Self::GetUpvalue,
            "SETUPVAL" => Self::SetUpvalue,
            "NEWTABLE" => Self::NewTable,
            "GETTABLE" => Self::GetTable,
            "SETTABLE" => Self::SetTable,
            "SELF" => Self::SelfCall,
            "SETLIST" => Self::SetList,
            "NOT" => Self::Not,
            "LEN" => Self::Len,
            "POW" => Self::Pow,
            "CONCAT" => Self::Concat,
            "EQ" => Self::Eq,
            "NE" => Self::Ne,
            "LT" => Self::Lt,
            "MOVE" => Self::Move,
            "ADD" => Self::Add,
            "SUB" => Self::Sub,
            "MUL" => Self::Mul,
            "DIV" => Self::Div,
            "XOR32" => Self::Xor32,
            "JUMP" => Self::Jump,
            "JUMP_TABLE" => Self::JumpTable,
            "FOR_NEXT" => Self::ForNext,
            "BR_TRUE" => Self::Branch,
            "BR_EQ" => Self::BranchEq,
            "BR_LT" => Self::BranchLt,
            "BR_LE" => Self::BranchLe,
            "VARARG" => Self::Vararg,
            "RETURN" => Self::Return,
            "RETURN_TOP" => Self::ReturnTop,
            "NATIVE" => Self::Native,
            "NATIVE_BRANCH" => Self::NativeBranch,
            "NATIVE_RETURN" => Self::NativeReturn,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Nop => "NOP",
            Self::LoadK => "LOADK",
            Self::GetGlobal => "GETGLOBAL",
            Self::Call => "CALL",
            Self::CallOpen => "CALL_OPEN",
            Self::CallFromTop => "CALL_FROM_TOP",
            Self::TailCall => "TAILCALL",
            Self::Closure => "CLOSURE",
            Self::CloseUpvalues => "CLOSEUPVALS",
            Self::GetUpvalue => "GETUPVAL",
            Self::SetUpvalue => "SETUPVAL",
            Self::NewTable => "NEWTABLE",
            Self::GetTable => "GETTABLE",
            Self::SetTable => "SETTABLE",
            Self::SelfCall => "SELF",
            Self::SetList => "SETLIST",
            Self::Not => "NOT",
            Self::Len => "LEN",
            Self::Pow => "POW",
            Self::Concat => "CONCAT",
            Self::Eq => "EQ",
            Self::Ne => "NE",
            Self::Lt => "LT",
            Self::Move => "MOVE",
            Self::Add => "ADD",
            Self::Sub => "SUB",
            Self::Mul => "MUL",
            Self::Div => "DIV",
            Self::Xor32 => "XOR32",
            Self::Jump => "JUMP",
            Self::JumpTable => "JUMP_TABLE",
            Self::ForNext => "FOR_NEXT",
            Self::Branch => "BR_TRUE",
            Self::BranchEq => "BR_EQ",
            Self::BranchLt => "BR_LT",
            Self::BranchLe => "BR_LE",
            Self::Vararg => "VARARG",
            Self::Return => "RETURN",
            Self::ReturnTop => "RETURN_TOP",
            Self::Native => "NATIVE",
            Self::NativeBranch => "NATIVE_BRANCH",
            Self::NativeReturn => "NATIVE_RETURN",
        }
    }

    fn signature(self) -> &'static [Type] {
        use Type::{Block, Const, Reg};
        match self {
            Self::Nop => &[],
            Self::LoadK | Self::GetGlobal | Self::GetUpvalue | Self::SetUpvalue => &[Reg, Const],
            Self::CloseUpvalues | Self::CallFromTop | Self::ReturnTop => &[Reg],
            Self::Move | Self::Not | Self::Len => &[Reg, Reg],
            Self::SelfCall => &[Reg, Reg, Reg, Reg],
            Self::Add
            | Self::Sub
            | Self::Mul
            | Self::Div
            | Self::Xor32
            | Self::GetTable
            | Self::SetTable
            | Self::Pow
            | Self::Concat
            | Self::Eq
            | Self::Ne
            | Self::Lt => &[Reg, Reg, Reg],
            Self::Jump => &[Block],
            Self::ForNext => &[Reg, Reg, Const, Block, Block],
            Self::Branch => &[Reg, Block, Block],
            Self::BranchEq | Self::BranchLt | Self::BranchLe => &[Reg, Reg, Block, Block],
            Self::Call
            | Self::CallOpen
            | Self::NewTable
            | Self::TailCall
            | Self::SetList
            | Self::JumpTable
            | Self::Closure
            | Self::Vararg
            | Self::Return
            | Self::Native
            | Self::NativeBranch
            | Self::NativeReturn => &[],
        }
    }

    pub fn types(self, count: usize) -> Result<Vec<Type>, String> {
        if matches!(self, Self::Call | Self::Closure | Self::SetList) {
            if count < 2 {
                return Err(format!(
                    "{} needs a register and a constant operand",
                    self.name()
                ));
            }
            let mut types = vec![Type::Reg; count];
            types[1] = Type::Const;
            if self == Self::Closure {
                types[2..].fill(Type::Capture);
            }
            return Ok(types);
        }
        if self == Self::JumpTable {
            if count < 2 {
                return Err("JUMP_TABLE needs a register and at least one target".into());
            }
            let mut types = vec![Type::Block; count];
            types[0] = Type::Reg;
            return Ok(types);
        }
        if matches!(self, Self::TailCall | Self::CallOpen | Self::NewTable) && count == 0 {
            return Err(format!("{} needs a register", self.name()));
        }
        if matches!(
            self,
            Self::Return | Self::Vararg | Self::TailCall | Self::CallOpen | Self::NewTable
        ) {
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
                | Self::JumpTable
                | Self::ForNext
                | Self::Branch
                | Self::BranchEq
                | Self::BranchLt
                | Self::BranchLe
                | Self::Return
                | Self::ReturnTop
                | Self::TailCall
                | Self::NativeBranch
                | Self::NativeReturn
        )
    }

    pub(crate) fn native(self) -> bool {
        matches!(self, Self::Native | Self::NativeBranch | Self::NativeReturn)
    }

    pub(crate) fn uses_result_top(self) -> bool {
        matches!(self, Self::CallOpen | Self::CallFromTop | Self::ReturnTop)
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
            if !expected.accepts(arg.ty()) {
                return Err(format!(
                    "{} operand {} expects {expected:?}, got {}",
                    op.name(),
                    index + 1,
                    arg
                ));
            }
        }
        if op == Op::GetGlobal && !matches!(args[1], Operand::Const(Const::String(_))) {
            return Err("GETGLOBAL needs a byte string name".into());
        }
        if op == Op::SelfCall && args[0] == args[1] {
            return Err("SELF needs distinct method and receiver destinations".into());
        }
        if op == Op::ForNext && !matches!(args[2], Operand::Const(Const::Bool(_))) {
            return Err("FOR_NEXT needs a boolean step direction flag".into());
        }
        if op == Op::SetList
            && !matches!(args[1], Operand::Const(Const::Number(first))
            if first.fract() == 0.0 && first >= 0.0
                && first <= (1u64 << 53).saturating_sub(args.len().saturating_sub(3) as u64) as f64)
        {
            return Err(
                "SETLIST needs an exact nonnegative starting key and exact subsequent keys".into(),
            );
        }
        if op == Op::Closure
            && !matches!(&args[1], Operand::Const(Const::String(name))
                if !name.is_empty() && std::str::from_utf8(name).is_ok())
        {
            return Err("CLOSURE needs a UTF-8 prototype name".into());
        }
        if op == Op::Closure
            && args[2..]
                .iter()
                .any(|arg| !matches!(arg, Operand::Reg(_)) && inherited_capture(arg).is_none())
        {
            return Err(
                "CLOSURE captures must be registers or upvalue slots from 0 through 254".into(),
            );
        }
        if matches!(op, Op::GetUpvalue | Op::SetUpvalue)
            && !matches!(args[1], Operand::Const(Const::Number(slot))
                if (0.0..=254.0).contains(&slot) && slot.fract() == 0.0)
        {
            return Err("upvalue slot must be an integer from 0 through 254".into());
        }
        if op == Op::Call
            && !matches!(args[1], Operand::Const(Const::Number(count))
            if count.fract() == 0.0 && count >= 0.0 && count <= (args.len() - 2) as f64)
        {
            return Err(
                "CALL result count must be a nonnegative integer with one register per result"
                    .into(),
            );
        }
        let instruction = Self { op, args };
        if op == Op::Call {
            let (_, results, _) = instruction.call_parts();
            let unique: BTreeSet<_> = results.iter().filter_map(operand_register).collect();
            if unique.len() != results.len() {
                return Err("CALL result registers must be distinct".into());
            }
        }
        Ok(instruction)
    }

    pub(crate) fn call_parts(&self) -> (Reg, &[Operand], &[Operand]) {
        let (Operand::Reg(function), Operand::Const(Const::Number(count))) =
            (&self.args[0], &self.args[1])
        else {
            unreachable!("validated CALL operands");
        };
        let (results, args) = self.args[2..].split_at(*count as usize);
        (*function, results, args)
    }

    pub(crate) fn closure_target(&self) -> &str {
        let Operand::Const(Const::String(name)) = &self.args[1] else {
            unreachable!("validated prototype name")
        };
        std::str::from_utf8(name).unwrap()
    }

    pub(crate) fn upvalue_slot(&self) -> usize {
        let Operand::Const(Const::Number(slot)) = self.args[1] else {
            unreachable!("validated upvalue slot")
        };
        slot as usize
    }

    fn destination(&self) -> Option<Reg> {
        if self.op.terminates()
            || self.op.native()
            || matches!(
                self.op,
                Op::Nop
                    | Op::CloseUpvalues
                    | Op::Call
                    | Op::CallOpen
                    | Op::Vararg
                    | Op::SetUpvalue
                    | Op::SetTable
                    | Op::SetList
                    | Op::SelfCall
            )
        {
            return None;
        }
        let Operand::Reg(reg) = self.args[0] else {
            unreachable!("validated destination")
        };
        Some(reg)
    }

    pub(crate) fn result(&self, facts: &Facts) -> Option<Const> {
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
        if self.op == Op::Vararg {
            for reg in self.args.iter().filter_map(operand_register) {
                facts.remove(&reg);
            }
            return;
        }
        if self.op.native()
            || self.op.uses_result_top()
            || matches!(
                self.op,
                Op::Call
                    | Op::SetUpvalue
                    | Op::SetTable
                    | Op::SetList
                    | Op::SelfCall
                    | Op::JumpTable
                    | Op::ForNext
            )
            || (self.comparison().is_some() && self.condition(facts).is_none())
        {
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
            Op::JumpTable => return &self.args[1..],
            Op::ForNext => return &self.args[3..],
            Op::Return | Op::ReturnTop | Op::TailCall | Op::NativeReturn => return &[],
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

#[derive(Clone)]
pub struct Block {
    pub name: String,
    pub instructions: Vec<Inst>,
}

#[derive(Clone)]
pub struct Proto {
    pub name: String,
    pub params: u8,
    pub variadic: bool,
    pub blocks: Vec<Block>,
    pub(crate) lua51: Option<crate::binary::Lua51Origin>,
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
            if !self.variadic && before.iter().any(|inst| inst.op == Op::Vararg) {
                return Err(format!(
                    "@{} uses VARARG in a non-variadic prototype",
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

    pub fn control_flow(&self) -> Result<Vec<Vec<usize>>, String> {
        let names = self.block_indices()?;
        let unknown = Facts::new();
        Ok(self
            .blocks
            .iter()
            .map(|block| {
                block
                    .instructions
                    .last()
                    .unwrap()
                    .successors(&unknown)
                    .iter()
                    .map(|target| names[target.block()])
                    .collect()
            })
            .collect())
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

    pub fn thread_jumps(&mut self) -> Result<(), String> {
        self.verify()?;
        let redirects: BTreeMap<_, _> = self
            .blocks
            .iter()
            .filter_map(|block| {
                let [jump] = block.instructions.as_slice() else {
                    return None;
                };
                (jump.op == Op::Jump).then(|| (block.name.clone(), jump.args[0].block().to_owned()))
            })
            .collect();
        for tail in self
            .blocks
            .iter_mut()
            .filter_map(|block| block.instructions.last_mut())
        {
            if tail.op.native() {
                continue;
            }
            for argument in &mut tail.args {
                let Operand::Block(target) = argument else {
                    continue;
                };
                let mut cursor = target.as_str();
                let mut visited = BTreeSet::new();
                while let Some(next) = redirects.get(cursor) {
                    if !visited.insert(cursor) {
                        break;
                    }
                    cursor = next;
                }
                if !redirects.contains_key(cursor) && cursor != target {
                    *target = cursor.to_owned();
                }
            }
        }
        Ok(())
    }

    pub fn dce(&mut self) -> Result<(), String> {
        self.verify()?;
        // a native closure can make later writes visible through an escaped register cell.
        if self.blocks.iter().flat_map(|block| &block.instructions).any(|inst| {
            inst.op == Op::Closure || inst.op.uses_result_top() || inst.op.native() && matches!(&inst.args[0], Operand::Const(Const::String(name))
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
                        if inst.op == Op::Vararg {
                            for reg in inst.args.iter().filter_map(operand_register) {
                                live.remove(&reg);
                            }
                        } else if inst.op == Op::Call {
                            let (function, results, args) = inst.call_parts();
                            for result in results.iter().filter_map(operand_register) {
                                live.remove(&result);
                            }
                            live.insert(function);
                            live.extend(args.iter().filter_map(operand_register));
                        } else if inst.op.native() {
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

pub(crate) fn inherited_capture(operand: &Operand) -> Option<usize> {
    let Operand::Const(Const::Number(index)) = operand else {
        return None;
    };
    ((0.0..=254.0).contains(index) && index.fract() == 0.0).then_some(*index as usize)
}

// distinguish signed zeros at joins because subsequent division can observe the sign.
pub(crate) fn same_fact(left: &Const, right: &Const) -> bool {
    match (left, right) {
        (Const::Number(left), Const::Number(right)) => left.to_bits() == right.to_bits(),
        _ => left == right,
    }
}

impl Display for Proto {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "proto {}", self.name)?;
        if self.params != 0 {
            write!(f, " params {}", self.params)?;
        }
        if self.variadic {
            f.write_str(" vararg")?;
        }
        writeln!(f, " {{")?;
        for block in &self.blocks {
            writeln!(f, "  @{}:", block.name)?;
            for inst in &block.instructions {
                writeln!(f, "    {inst};")?;
            }
        }
        writeln!(f, "}}")
    }
}
