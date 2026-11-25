use crate::ir::{Comparison, Const, Facts, Inst, Op, Operand, Proto, Type, value_of};
use std::collections::BTreeMap;

#[derive(Clone)]
pub enum PatternArg {
    Bind(String),
    Literal(Operand),
}

pub struct PatternInst {
    pub op: Op,
    pub args: Vec<PatternArg>,
}

pub enum Term {
    Bound(String),
    Literal(Const),
}

pub enum Guard {
    Known(Term),
    Compare {
        left: Term,
        right: Term,
        comparison: Comparison,
    },
}

impl Term {
    fn resolve<'a>(&'a self, bindings: &'a Bindings, facts: &'a Facts) -> Option<&'a Const> {
        match self {
            Self::Bound(name) => value_of(bindings.get(name.as_str())?, facts),
            Self::Literal(value) => Some(value),
        }
    }

    fn check(&self, types: &BTreeMap<&str, Type>) -> Result<(), String> {
        if let Self::Bound(name) = self {
            match types.get(name.as_str()) {
                Some(Type::Reg | Type::Const | Type::Capture) => {}
                Some(Type::Block) => return Err(format!("${name} is a block, not a value")),
                None => return Err(format!("unknown binding ${name}")),
            }
        }
        Ok(())
    }
}

impl Guard {
    fn check(&self, types: &BTreeMap<&str, Type>) -> Result<(), String> {
        match self {
            Self::Known(term) => term.check(types),
            Self::Compare { left, right, .. } => {
                left.check(types)?;
                right.check(types)
            }
        }
    }

    fn satisfied(&self, bindings: &Bindings, facts: &Facts) -> bool {
        match self {
            Self::Known(term) => term.resolve(bindings, facts).is_some(),
            Self::Compare {
                left,
                right,
                comparison,
            } => {
                left.resolve(bindings, facts)
                    .zip(right.resolve(bindings, facts))
                    .and_then(|(a, b)| comparison.evaluate(a, b))
                    == Some(true)
            }
        }
    }
}

type Bindings<'a> = BTreeMap<&'a str, Operand>;
type SiteFacts = Vec<Vec<Option<Facts>>>;

pub struct Rule {
    pub pattern: Vec<PatternInst>,
    pub arms: Vec<Arm>,
}

pub struct Arm {
    pub guards: Vec<Guard>,
    pub replacement: Vec<PatternInst>,
}

impl Rule {
    pub fn check(&self) -> Result<(), String> {
        if self.pattern.is_empty() {
            return Err("match requires at least one instruction".into());
        }
        let mut types = BTreeMap::new();
        for (instructions, declares) in std::iter::once((&self.pattern, true))
            .chain(self.arms.iter().map(|arm| (&arm.replacement, false)))
        {
            for inst in instructions {
                for (arg, required) in inst.args.iter().zip(inst.op.types(inst.args.len())?) {
                    match arg {
                        PatternArg::Bind(name) => {
                            let existing = if declares {
                                Some(*types.entry(name.as_str()).or_insert(required))
                            } else {
                                types.get(name.as_str()).copied()
                            };
                            match existing {
                                Some(ty) if required.accepts(ty) => {}
                                Some(ty) if declares && ty.accepts(required) => {
                                    types.insert(name.as_str(), required);
                                }
                                Some(ty) => {
                                    return Err(format!(
                                        "${name} has type {ty:?}, expected {required:?}"
                                    ));
                                }
                                None => return Err(format!("unknown binding ${name}")),
                            }
                        }
                        PatternArg::Literal(value) if !required.accepts(value.ty()) => {
                            return Err(format!(
                                "{} expects {required:?}, got {value}",
                                inst.op.name()
                            ));
                        }
                        PatternArg::Literal(_) => {}
                    }
                }
            }
        }
        for (index, arm) in self.arms.iter().enumerate() {
            if arm.guards.is_empty() && index + 1 != self.arms.len() {
                return Err("an unconditional match arm must come last".into());
            }
            for guard in &arm.guards {
                guard.check(&types)?;
            }
        }
        Ok(())
    }

    fn bindings(&self, instructions: &[Inst]) -> Option<Bindings<'_>> {
        let mut bindings = BTreeMap::<&str, Operand>::new();
        for (expected, actual) in self.pattern.iter().zip(instructions) {
            if expected.op != actual.op || expected.args.len() != actual.args.len() {
                return None;
            }
            for (expected, actual) in expected.args.iter().zip(&actual.args) {
                match expected {
                    PatternArg::Literal(value) if !value.same_repr(actual) => return None,
                    PatternArg::Literal(_) => {}
                    PatternArg::Bind(name) => {
                        let old = bindings.entry(name).or_insert_with(|| actual.clone());
                        if !old.same_repr(actual) {
                            return None;
                        }
                    }
                }
            }
        }
        Some(bindings)
    }

    fn render(arm: &Arm, bindings: &Bindings) -> Result<Vec<Inst>, String> {
        arm.replacement
            .iter()
            .map(|inst| {
                let args = inst
                    .args
                    .iter()
                    .map(|arg| match arg {
                        PatternArg::Literal(value) => value.clone(),
                        PatternArg::Bind(name) => bindings[name.as_str()].clone(),
                    })
                    .collect();
                Inst::new(inst.op, args)
            })
            .collect()
    }

    fn apply(&self, proto: &mut Proto, mut facts: Option<SiteFacts>) -> Result<(), String> {
        for block in 0..proto.blocks.len() {
            let mut start = 0;
            while let Some(instructions) = proto.blocks[block]
                .instructions
                .get(start..start + self.pattern.len())
            {
                let Some(bindings) = self.bindings(instructions) else {
                    start += 1;
                    continue;
                };
                let before = facts
                    .as_ref()
                    .and_then(|sites| sites[block][start].as_ref());
                let Some(arm) = self.arms.iter().find(|arm| {
                    arm.guards.is_empty()
                        || before.is_some_and(|before| {
                            arm.guards
                                .iter()
                                .all(|guard| guard.satisfied(&bindings, before))
                        })
                }) else {
                    start += 1;
                    continue;
                };
                let replacement = Self::render(arm, &bindings)?;
                let inserted = replacement.len();
                let end = start + self.pattern.len();
                proto.blocks[block]
                    .instructions
                    .splice(start..end, replacement);
                proto.verify()?;
                if facts.is_some() {
                    facts = Some(proto.facts()?);
                }
                start += inserted;
            }
        }
        Ok(())
    }
}

pub enum Step {
    Fold,
    Rewrite(Rule),
    Thread,
    Prune,
    Dce,
}

pub struct Pass {
    pub name: String,
    pub steps: Vec<Step>,
}

impl Pass {
    pub fn run(&self, proto: &mut Proto) -> Result<(), String> {
        for step in &self.steps {
            let result = match step {
                Step::Fold => proto.fold_constants(),
                Step::Thread => proto.thread_jumps(),
                Step::Prune => proto.prune_unreachable(),
                Step::Dce => proto.dce(),
                Step::Rewrite(rule) => {
                    let facts = rule
                        .arms
                        .iter()
                        .any(|arm| !arm.guards.is_empty())
                        .then(|| proto.facts())
                        .transpose()?;
                    rule.apply(proto, facts)
                }
            };
            result.map_err(|error| {
                format!("pass {}, prototype {}: {error}", self.name, proto.name)
            })?;
        }
        Ok(())
    }
}

pub enum MapArg {
    Slot(usize),
    Literal(Operand),
}

pub struct Mapping {
    pub op: Op,
    pub args: Vec<MapArg>,
}

impl Mapping {
    pub fn check(&self) -> Result<(), String> {
        let mut types = BTreeMap::new();
        for (arg, required) in self.args.iter().zip(self.op.types(self.args.len())?) {
            match arg {
                MapArg::Slot(index) => {
                    let previous = types.entry(*index).or_insert(required);
                    if previous.accepts(required) {
                        *previous = required;
                    } else if !required.accepts(*previous) {
                        return Err(format!("raw operand ${index} has conflicting types"));
                    }
                }
                MapArg::Literal(value) if !required.accepts(value.ty()) => {
                    return Err(format!(
                        "{} expects {required:?}, got {value}",
                        self.op.name()
                    ));
                }
                MapArg::Literal(_) => {}
            }
        }
        Ok(())
    }

    pub fn lift(&self, raw: &[Operand]) -> Result<Inst, String> {
        let args = self
            .args
            .iter()
            .map(|arg| match arg {
                MapArg::Slot(slot) => raw
                    .get(*slot)
                    .cloned()
                    .ok_or_else(|| format!("missing raw operand ${slot}")),
                MapArg::Literal(value) => Ok(value.clone()),
            })
            .collect::<Result<Vec<_>, _>>()?;
        Inst::new(self.op, args)
    }
}

#[derive(Default)]
pub struct Script {
    pub maps: BTreeMap<String, BTreeMap<u32, Mapping>>,
    pub passes: Vec<Pass>,
}
