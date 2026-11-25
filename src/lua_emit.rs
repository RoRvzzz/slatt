use crate::ir::{Const, Inst, Op, Operand, Proto, Reg, inherited_capture};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt::Write as _;

pub(crate) fn lua_string(bytes: &[u8]) -> String {
    let mut text = String::from("\"");
    for &byte in bytes {
        match byte {
            b'"' => text.push_str("\\\""),
            b'\\' => text.push_str("\\\\"),
            32..=126 => text.push(char::from(byte)),
            _ => write!(text, "\\{byte:03}").unwrap(),
        }
    }
    text.push('"');
    text
}

fn constant(value: &Const) -> Result<String, String> {
    Ok(match value {
        Const::Nil => "nil".into(),
        Const::Bool(flag) => flag.to_string(),
        Const::String(bytes) => lua_string(bytes),
        Const::Number(number) if number.is_finite() => {
            if *number == 0.0 && number.is_sign_negative() {
                "-0.0".into()
            } else {
                number.to_string()
            }
        }
        Const::Number(number) if number.is_infinite() => {
            if number.is_sign_negative() {
                "(-1 / 0)".into()
            } else {
                "(1 / 0)".into()
            }
        }
        Const::Number(_) => return Err("Lua source cannot preserve a NaN bit pattern".into()),
    })
}

fn global(bytes: &[u8]) -> Option<&str> {
    let name = std::str::from_utf8(bytes).ok()?;
    let mut chars = name.bytes();
    if !chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == b'_')
        || !chars.all(|c| c.is_ascii_alphanumeric() || c == b'_')
        || [
            "and", "break", "const", "continue", "do", "else", "elseif", "end", "export", "false",
            "for", "function", "goto", "if", "in", "local", "nil", "not", "or", "repeat", "return",
            "then", "true", "type", "until", "while",
        ]
        .contains(&name)
    {
        // Luau's contextual keywords `type`, `export`, `const` and `continue` still
        // permit expression uses; Lua 5.1 treats all of them as identifiers.
        if ["type", "export", "const", "continue"].contains(&name) {
            return Some(name);
        }
        return None;
    }
    Some(name)
}

struct Writer<'a> {
    proto: &'a Proto,
    graph: Vec<Vec<usize>>,
    prefix: String,
    register_table: bool,
    managed_captures: bool,
    text: String,
}

impl Writer<'_> {
    fn line(&mut self, depth: usize, text: &str) {
        for _ in 0..depth {
            self.text.push_str("    ");
        }
        self.text.push_str(text);
        self.text.push('\n');
    }

    fn register(&self, reg: Reg) -> String {
        if self.register_table {
            format!("{}_slots[{}]", self.prefix, reg.0)
        } else {
            format!("{}{}", self.prefix, reg.0)
        }
    }

    fn operand(&self, value: &Operand) -> Result<String, String> {
        match value {
            Operand::Reg(reg) => Ok(self.register(*reg)),
            Operand::Const(value) => constant(value),
            Operand::Block(_) => Err("control-flow targets cannot be emitted as values".into()),
        }
    }

    fn list(&self, values: &[Operand]) -> Result<String, String> {
        Ok(values
            .iter()
            .map(|value| self.operand(value))
            .collect::<Result<Vec<_>, _>>()?
            .join(", "))
    }

    fn statement(&self, inst: &Inst) -> Result<String, String> {
        let lhs = || self.operand(&inst.args[0]);
        let arg = |index: usize| self.operand(&inst.args[index]);
        match inst.op {
            Op::Nop => Ok(String::new()),
            Op::LoadK | Op::Move => Ok(format!("{} = {}", lhs()?, arg(1)?)),
            Op::GetGlobal => {
                let Operand::Const(Const::String(name)) = &inst.args[1] else {
                    unreachable!("validated global name");
                };
                let lookup = global(name).map(str::to_owned).unwrap_or_else(|| {
                    format!("((getfenv and getfenv()) or _ENV)[{}]", lua_string(name))
                });
                Ok(format!("{} = {lookup}", lhs()?))
            }
            Op::Call => {
                let (callee, results, args) = inst.call_parts();
                let call = format!("{}({})", self.register(callee), self.list(args)?);
                if results.is_empty() {
                    Ok(call)
                } else {
                    Ok(format!("{} = {call}", self.list(results)?))
                }
            }
            Op::CallOpen | Op::CallFromTop | Op::ReturnTop => {
                let Operand::Reg(base) = inst.args[0] else {
                    unreachable!("validated call base")
                };
                let base = usize::from(base.0);
                let p = &self.prefix;
                Ok(match inst.op {
                    Op::CallOpen => format!(
                        "do local count, results = {p}_pack({p}_slots[{base}]({})); {p}_top = {base} + count - 1; for i = 1, count do {p}_slots[{base} + i - 1] = results[i] end end",
                        self.list(&inst.args[1..])?
                    ),
                    Op::CallFromTop => format!(
                        "{p}_slots[{base}] = {p}_slots[{base}]({p}_unpack({p}_slots, {}, {p}_top))",
                        base + 1
                    ),
                    _ => format!("return {p}_unpack({p}_slots, {base}, {p}_top)"),
                })
            }
            Op::TailCall => Ok(format!(
                "return {}({})",
                arg(0)?,
                self.list(&inst.args[1..])?
            )),
            Op::Closure => {
                let captures = inst.args[2..]
                    .iter()
                    .map(|capture| match capture {
                        Operand::Reg(reg) if self.managed_captures => {
                            format!("{}_capture({})", self.prefix, reg.0)
                        }
                        Operand::Reg(reg) => format!("{{{}_slots, {}}}", self.prefix, reg.0),
                        _ => format!(
                            "{}_up[{}]",
                            self.prefix,
                            inherited_capture(capture).unwrap() + 1
                        ),
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                Ok(format!(
                    "{} = {}_prototypes[{}]({{{captures}}})",
                    lhs()?,
                    self.prefix,
                    lua_string(inst.closure_target().as_bytes())
                ))
            }
            Op::CloseUpvalues if !self.managed_captures => Ok(String::new()),
            Op::CloseUpvalues => {
                let Operand::Reg(first) = inst.args[0] else {
                    unreachable!("validated close register")
                };
                Ok(format!("{}_close({})", self.prefix, first.0))
            }
            Op::GetUpvalue | Op::SetUpvalue => {
                let capture = format!("{}_up[{}]", self.prefix, inst.upvalue_slot() + 1);
                let value = format!("{capture}[1][{capture}[2]]");
                if inst.op == Op::GetUpvalue {
                    Ok(format!("{} = {value}", lhs()?))
                } else {
                    Ok(format!("{value} = {}", lhs()?))
                }
            }
            Op::NewTable => Ok(format!("{} = {{{}}}", lhs()?, self.list(&inst.args[1..])?)),
            Op::GetTable => Ok(format!("{} = {}[{}]", lhs()?, arg(1)?, arg(2)?)),
            Op::SetTable => Ok(format!("{}[{}] = {}", lhs()?, arg(1)?, arg(2)?)),
            Op::SelfCall => {
                let receiver = format!("{}_receiver", self.prefix);
                let method = format!("{}_method", self.prefix);
                Ok(format!(
                    "do local {receiver} = {}; local {method} = {receiver}[{}]; {} = {receiver}; {} = {method} end",
                    arg(2)?,
                    arg(3)?,
                    arg(1)?,
                    lhs()?
                ))
            }
            Op::SetList => {
                if inst.args.len() == 2 {
                    return Ok(String::new());
                }
                let Operand::Const(Const::Number(first)) = inst.args[1] else {
                    unreachable!("validated list key");
                };
                let table = format!("{}_list", self.prefix);
                let mut text = format!("do local {table} = {};", lhs()?);
                for (offset, value) in inst.args[2..].iter().enumerate() {
                    let key = first as u64 + offset as u64;
                    write!(text, " {table}[{key}] = {};", self.operand(value)?).unwrap();
                }
                text.push_str(" end");
                Ok(text)
            }
            Op::Not => Ok(format!("{} = not {}", lhs()?, arg(1)?)),
            Op::Len => Ok(format!("{} = #{}", lhs()?, arg(1)?)),
            Op::Add
            | Op::Sub
            | Op::Mul
            | Op::Div
            | Op::Pow
            | Op::Concat
            | Op::Eq
            | Op::Ne
            | Op::Lt => {
                let operator = match inst.op {
                    Op::Add => "+",
                    Op::Sub => "-",
                    Op::Mul => "*",
                    Op::Div => "/",
                    Op::Pow => "^",
                    Op::Concat => "..",
                    Op::Eq => "==",
                    Op::Ne => "~=",
                    Op::Lt => "<",
                    _ => unreachable!(),
                };
                Ok(format!("{} = {} {operator} {}", lhs()?, arg(1)?, arg(2)?))
            }
            Op::Vararg if inst.args.is_empty() => Ok(String::new()),
            Op::Vararg => Ok(format!("{} = ...", self.list(&inst.args)?)),
            Op::Return if inst.args.is_empty() => Ok("return".into()),
            Op::Return => Ok(format!("return {}", self.list(&inst.args)?)),
            Op::Xor32 => Err("XOR32 needs known operands for portable Lua source".into()),
            _ => Err(format!("cannot emit opaque instruction {inst}")),
        }
    }

    fn condition(&self, inst: &Inst) -> Result<String, String> {
        let left = self.operand(&inst.args[0])?;
        let op = match inst.op {
            Op::Branch => return Ok(left),
            Op::BranchEq => "==",
            Op::BranchLt => "<",
            Op::BranchLe => "<=",
            _ => unreachable!("validated conditional terminator"),
        };
        Ok(format!("{left} {op} {}", self.operand(&inst.args[1])?))
    }

    fn statements(&mut self, id: usize, depth: usize) -> Result<(), String> {
        let block = &self.proto.blocks[id];
        for (index, inst) in block.instructions[..block.instructions.len() - 1]
            .iter()
            .enumerate()
        {
            let line = self
                .statement(inst)
                .map_err(|error| format!("@{}:{index}: {error}", block.name))?;
            if !line.is_empty() {
                self.line(depth, &line);
            }
        }
        Ok(())
    }

    fn region(
        &mut self,
        mut id: usize,
        stop: usize,
        depth: usize,
        joins: &[usize],
        budget: &mut usize,
    ) -> Result<bool, String> {
        while id != stop {
            if *budget == 0 || depth >= 80 {
                return Ok(false);
            }
            *budget -= 1;
            self.statements(id, depth)?;
            let tail = self.proto.blocks[id].instructions.last().unwrap();
            match self.graph[id].as_slice() {
                [] => {
                    self.line(depth, &self.statement(tail)?);
                    break;
                }
                [next] => id = *next,
                [yes, no] => {
                    let (yes, no, join) = (*yes, *no, joins[id]);
                    self.line(depth, &format!("if {} then", self.condition(tail)?));
                    if !self.region(yes, join, depth + 1, joins, budget)? {
                        return Ok(false);
                    }
                    if no != join {
                        self.line(depth, "else");
                        if !self.region(no, join, depth + 1, joins, budget)? {
                            return Ok(false);
                        }
                    }
                    self.line(depth, "end");
                    id = join;
                }
                _ => unreachable!("validated PFIR exits"),
            }
        }
        Ok(true)
    }

    fn dispatch(&mut self, depth: usize) -> Result<(), String> {
        let pc = format!("{}_block", self.prefix);
        let mut tables = BTreeMap::new();
        for id in 0..self.proto.blocks.len() {
            let tail = self.proto.blocks[id].instructions.last().unwrap();
            if tail.op == Op::JumpTable && !tables.contains_key(&self.graph[id]) {
                let table = format!("{}_targets{}", self.prefix, tables.len());
                let targets = &self.graph[id];
                if targets.len() > 16 && targets.windows(2).all(|pair| pair[1] == pair[0] + 1) {
                    let populate = format!(
                        "for i = 1, {} do {table}[i] = i + {} end",
                        targets.len(),
                        targets[0] as i64 - 1
                    );
                    self.line(depth, &format!("local {table} = {{}}"));
                    self.line(depth, &populate);
                } else {
                    let values = targets
                        .iter()
                        .map(usize::to_string)
                        .collect::<Vec<_>>()
                        .join(", ");
                    self.line(depth, &format!("local {table} = {{{values}}}"));
                }
                tables.insert(self.graph[id].clone(), table);
            }
        }
        self.line(depth, &format!("local {pc} = 0"));
        self.line(depth, "while true do");
        self.dispatch_range(depth + 1, &pc, &tables, 0..self.proto.blocks.len())?;
        self.line(depth, "end");
        Ok(())
    }

    fn dispatch_range(
        &mut self,
        depth: usize,
        pc: &str,
        tables: &BTreeMap<Vec<usize>, String>,
        ids: std::ops::Range<usize>,
    ) -> Result<(), String> {
        let end = ids.end;
        let direct_end = if ids.start == 0 && ids.len() > 16 {
            // Entry and early loop headers are common hot blocks.
            4
        } else {
            end
        };
        if direct_end == end && ids.len() > 16 {
            let mid = ids.start + ids.len() / 2;
            self.line(depth, &format!("if {pc} < {mid} then"));
            self.dispatch_range(depth + 1, pc, tables, ids.start..mid)?;
            self.line(depth, "else");
            self.dispatch_range(depth + 1, pc, tables, mid..ids.end)?;
            self.line(depth, "end");
            return Ok(());
        }
        for (index, id) in (ids.start..direct_end).enumerate() {
            let keyword = if index == 0 { "if" } else { "elseif" };
            self.line(depth, &format!("{keyword} {pc} == {id} then"));
            self.statements(id, depth + 1)?;
            let tail = self.proto.blocks[id].instructions.last().unwrap();
            if tail.op == Op::JumpTable {
                let table = &tables[&self.graph[id]];
                let key = self.operand(&tail.args[0])?;
                self.line(depth + 1, &format!("{pc} = {table}[{key} + 1]"));
                self.line(
                    depth + 1,
                    &format!("if {pc} == nil then error(\"invalid computed jump\") end"),
                );
                continue;
            }
            if tail.op == Op::ForNext {
                let [yes, no] = self.graph[id].as_slice() else {
                    unreachable!("validated numeric loop exits")
                };
                let (yes, no) = (*yes, *no);
                let loop_state = format!("{}_loop", self.prefix);
                let state = self.operand(&tail.args[0])?;
                let iterator = self.operand(&tail.args[1])?;
                self.line(depth + 1, &format!("local {loop_state} = {state}"));
                let comparison = if tail.args[2] == Operand::Const(Const::Bool(true)) {
                    self.line(
                        depth + 1,
                        &format!(
                            "local positive, negative = {loop_state}[3] > 0, {loop_state}[3] < 0"
                        ),
                    );
                    format!(
                        "(positive and {loop_state}[1] <= {loop_state}[2]) or (negative and {loop_state}[1] >= {loop_state}[2])"
                    )
                } else {
                    format!("{loop_state}[1] <= {loop_state}[2]")
                };
                self.line(depth + 1, &format!("if {comparison} then"));
                self.line(depth + 2, &format!("local value, following = {loop_state}[1], ({loop_state}[1] + {loop_state}[3])"));
                self.line(depth + 2, &format!("{iterator} = value"));
                self.line(depth + 2, &format!("{loop_state}[1] = following"));
                self.line(depth + 2, &format!("{pc} = {yes}"));
                self.line(depth + 1, "else");
                self.line(depth + 2, &format!("{pc} = {no}"));
                self.line(depth + 1, "end");
                continue;
            }
            match self.graph[id].as_slice() {
                [] => self.line(depth + 1, &self.statement(tail)?),
                [next] => self.line(depth + 1, &format!("{pc} = {next}")),
                [yes, no] => self.line(
                    depth + 1,
                    &format!("{pc} = ({}) and {yes} or {no}", self.condition(tail)?),
                ),
                _ => unreachable!("validated PFIR exits"),
            }
        }
        if direct_end < end {
            self.line(depth, "else");
            self.dispatch_range(depth + 1, pc, tables, direct_end..end)?;
        }
        self.line(depth, "end");
        Ok(())
    }

    fn natural_loop(&mut self, shape: LoopShape, depth: usize) -> Result<bool, String> {
        for id in shape.prefix {
            self.statements(id, depth)?;
        }
        self.line(depth, "while true do");
        self.statements(shape.header, depth + 1)?;
        let tail = self.proto.blocks[shape.header].instructions.last().unwrap();
        let condition = self.condition(tail)?;
        let exit_condition = if shape.repeat_on_true {
            format!("not ({condition})")
        } else {
            condition
        };
        self.line(depth + 1, &format!("if {exit_condition} then break end"));
        for id in shape.body {
            self.statements(id, depth + 1)?;
        }
        self.line(depth, "end");
        let nodes = self.graph.len();
        self.region(shape.exit, nodes, depth, &shape.joins, &mut (8 * nodes + 8))
    }
}

struct LoopShape {
    prefix: Vec<usize>,
    header: usize,
    repeat_on_true: bool,
    body: Vec<usize>,
    exit: usize,
    joins: Vec<usize>,
}

fn simple_loop(graph: &[Vec<usize>]) -> Option<LoopShape> {
    let mut prefix = Vec::new();
    let mut seen = BTreeSet::new();
    let mut header = 0;
    while let [next] = graph[header].as_slice() {
        if !seen.insert(header) {
            return None;
        }
        prefix.push(header);
        header = *next;
    }
    if !seen.insert(header) {
        return None;
    }
    let [yes, no] = *graph[header].as_slice() else {
        return None;
    };
    for (repeat_on_true, start, exit) in [(true, yes, no), (false, no, yes)] {
        let mut used = seen.clone();
        let mut body = Vec::new();
        let mut id = start;
        while id != header && used.insert(id) {
            let [next] = graph[id].as_slice() else {
                break;
            };
            body.push(id);
            id = *next;
        }
        if id != header || used.contains(&exit) {
            continue;
        }
        if graph
            .iter()
            .enumerate()
            .any(|(from, edges)| !used.contains(&from) && edges.iter().any(|to| used.contains(to)))
        {
            continue;
        }
        let remainder: Vec<_> = graph
            .iter()
            .enumerate()
            .map(|(id, edges)| {
                if used.contains(&id) {
                    Vec::new()
                } else {
                    edges.clone()
                }
            })
            .collect();
        if let Some(joins) = postdominators(&remainder) {
            return Some(LoopShape {
                prefix,
                header,
                repeat_on_true,
                body,
                exit,
                joins,
            });
        }
    }
    None
}

fn postdominators(graph: &[Vec<usize>]) -> Option<Vec<usize>> {
    let exit = graph.len();
    let mut incoming = vec![0usize; exit];
    for &target in graph.iter().flatten() {
        incoming[target] += 1;
    }
    let mut pending: VecDeque<_> = (0..exit).filter(|&id| incoming[id] == 0).collect();
    let mut order = Vec::new();
    while let Some(id) = pending.pop_front() {
        order.push(id);
        for &next in &graph[id] {
            incoming[next] -= 1;
            if incoming[next] == 0 {
                pending.push_back(next);
            }
        }
    }
    if order.len() != exit {
        return None;
    }
    let mut joins = vec![exit; exit + 1];
    let mut depth = vec![0; exit + 1];
    // In a DAG each successor's postdominator chain is already complete.
    for id in order.into_iter().rev() {
        let join = match graph[id].as_slice() {
            [] => exit,
            [one] => *one,
            [left, right] => {
                let (mut a, mut b) = (*left, *right);
                while a != b {
                    if depth[a] >= depth[b] {
                        a = joins[a];
                    } else {
                        b = joins[b];
                    }
                }
                a
            }
            _ => unreachable!("validated PFIR exits"),
        };
        joins[id] = join;
        depth[id] = depth[join] + 1;
    }
    Some(joins)
}

pub fn emit_program(program: &[Proto]) -> Result<String, String> {
    let root = program.first().ok_or("missing root prototype")?;
    let mut upvalues = BTreeMap::new();
    let mut globals = BTreeSet::new();
    let mut linked = program.len() > 1;
    for proto in program {
        proto.verify()?;
        let mut slots = 0;
        for inst in proto.blocks.iter().flat_map(|block| &block.instructions) {
            match inst.op {
                Op::GetGlobal => {
                    let Operand::Const(Const::String(name)) = &inst.args[1] else {
                        unreachable!("validated global name");
                    };
                    globals.extend(global(name));
                }
                Op::Closure => {
                    linked = true;
                    for slot in inst.args[2..].iter().filter_map(inherited_capture) {
                        slots = slots.max(slot + 1);
                    }
                }
                Op::GetUpvalue | Op::SetUpvalue => {
                    slots = slots.max(inst.upvalue_slot() + 1);
                }
                _ => {}
            }
        }
        if upvalues.insert(proto.name.as_str(), slots).is_some() {
            return Err(format!("duplicate prototype {}", proto.name));
        }
    }
    if upvalues[root.name.as_str()] != 0 {
        return Err(format!("root prototype {} cannot use upvalues", root.name));
    }
    let mut capture_counts = BTreeMap::new();
    for proto in program {
        for inst in proto.blocks.iter().flat_map(|block| &block.instructions) {
            if inst.op != Op::Closure {
                continue;
            }
            let target = inst.closure_target();
            let required = upvalues.get(target).ok_or_else(|| {
                format!(
                    "prototype {} references missing prototype {target}",
                    proto.name
                )
            })?;
            let count = inst.args.len() - 2;
            if count > 255
                || count < *required
                || capture_counts
                    .insert(target, count)
                    .is_some_and(|previous| previous != count)
            {
                return Err(format!(
                    "invalid capture count {count} for prototype {target}, requires at least {required}"
                ));
            }
        }
    }
    let mut prefix = String::from("r");
    while globals.iter().any(|name| name.starts_with(&prefix)) {
        prefix.insert(0, '_');
    }
    if !linked {
        return emit_prototype(root, &prefix, false);
    }
    let mut text = format!("local {prefix}_prototypes = {{}}\n");
    for proto in program {
        text.push_str(
            &emit_prototype(proto, &prefix, true)
                .map_err(|error| format!("prototype {}: {error}", proto.name))?,
        );
    }
    writeln!(
        text,
        "return {prefix}_prototypes[{}]({{}})(...)",
        lua_string(root.name.as_bytes())
    )
    .unwrap();
    Ok(text)
}

pub fn emit(input: &Proto) -> Result<String, String> {
    emit_program(std::slice::from_ref(input))
}

fn emit_prototype(input: &Proto, prefix: &str, linked: bool) -> Result<String, String> {
    let mut proto = input.clone();
    proto.prune_unreachable()?;
    proto.fold_constants()?;
    let mut registers = BTreeSet::new();
    let mut captured = BTreeSet::new();
    let mut closes = false;
    let mut opens_results = false;
    let mut unpacks_top = false;
    for block in &proto.blocks {
        for (pc, inst) in block.instructions.iter().enumerate() {
            if inst.op.native() {
                return Err(format!(
                    "@{}:{pc}: cannot emit opaque instruction {inst}",
                    block.name
                ));
            }
            if inst.op == Op::Closure {
                captured.extend(inst.args[2..].iter().filter_map(|arg| match arg {
                    Operand::Reg(reg) => Some(reg.0),
                    _ => None,
                }));
            }
            closes |= inst.op == Op::CloseUpvalues;
            opens_results |= inst.op == Op::CallOpen;
            unpacks_top |= matches!(inst.op, Op::CallFromTop | Op::ReturnTop);
            registers.extend(inst.args.iter().filter_map(|arg| match arg {
                Operand::Reg(reg) => Some(reg.0),
                _ => None,
            }));
        }
    }
    if proto.params > 195 {
        return Err("portable Lua source supports up to 195 fixed parameters".into());
    }
    registers.retain(|&reg| reg >= u16::from(proto.params));
    let register_table = !captured.is_empty()
        || opens_results
        || unpacks_top
        || registers.len() + usize::from(proto.params) > 195;
    let managed_captures = !captured.is_empty() && closes;
    let depth = if linked {
        2
    } else {
        usize::from(proto.params > 0)
    };
    let mut writer = Writer {
        proto: &proto,
        graph: proto.control_flow()?,
        prefix: prefix.into(),
        register_table,
        managed_captures,
        text: String::new(),
    };
    let parameters: Vec<_> = (0..proto.params)
        .map(|reg| format!("{}{reg}", writer.prefix))
        .collect();
    if depth > 0 {
        let mut signature = parameters.clone();
        if proto.variadic {
            signature.push("...".into());
        }
        if linked {
            writer.line(
                0,
                &format!(
                    "{prefix}_prototypes[{}] = function({prefix}_up)",
                    lua_string(proto.name.as_bytes())
                ),
            );
            writer.line(1, &format!("return function({})", signature.join(", ")));
        } else {
            writer.line(
                0,
                &format!("local function {prefix}_entry({})", signature.join(", ")),
            );
        }
    }
    if register_table {
        let initialized = parameters
            .iter()
            .enumerate()
            .map(|(index, name)| format!("[{index}] = {name}"))
            .collect::<Vec<_>>();
        writer.line(
            depth,
            &format!(
                "local {}_slots = {{{}}}",
                writer.prefix,
                initialized.join(", ")
            ),
        );
    } else if !registers.is_empty() {
        let locals = registers
            .iter()
            .map(|&reg| writer.register(Reg(reg)))
            .collect::<Vec<_>>();
        writer.line(depth, &format!("local {}", locals.join(", ")));
    }
    if managed_captures {
        writer.line(depth, &format!("local {prefix}_open = {{}}"));
        writer.line(depth, &format!("local function {prefix}_capture(slot)"));
        writer.line(depth + 1, &format!("local cell = {prefix}_open[slot]"));
        writer.line(depth + 1, &format!("if cell == nil then cell = {{{prefix}_slots, slot}}; {prefix}_open[slot] = cell end"));
        writer.line(depth + 1, "return cell");
        writer.line(depth, "end");
        let slots = captured
            .iter()
            .map(u16::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        writer.line(depth, &format!("local {prefix}_captured = {{{slots}}}"));
        writer.line(depth, &format!("local function {prefix}_close(first)"));
        writer.line(depth + 1, &format!("for i = 1, #{prefix}_captured do"));
        writer.line(depth + 2, &format!("local slot = {prefix}_captured[i]"));
        writer.line(depth + 2, &format!("local cell = {prefix}_open[slot]"));
        writer.line(depth + 2, "if slot >= first and cell ~= nil then");
        writer.line(
            depth + 3,
            &format!("cell[1], cell[2] = {{{prefix}_slots[slot]}}, 1"),
        );
        writer.line(depth + 3, &format!("{prefix}_open[slot] = nil"));
        writer.line(depth + 2, "end");
        writer.line(depth + 1, "end");
        writer.line(depth, "end");
    }
    if opens_results || unpacks_top {
        writer.line(depth, &format!("local {prefix}_top = -1"));
    }
    if opens_results {
        writer.line(depth, &format!("local {prefix}_select = select"));
        writer.line(
            depth,
            &format!(
                "local function {prefix}_pack(...) return {prefix}_select(\"#\", ...), {{...}} end"
            ),
        );
    }
    if unpacks_top {
        writer.line(
            depth,
            &format!("local {prefix}_unpack = table.unpack or unpack"),
        );
    }
    let mark = writer.text.len();
    let needs_dispatch = proto.blocks.iter().any(|block| {
        matches!(
            block.instructions.last().unwrap().op,
            Op::JumpTable | Op::ForNext
        )
    });
    let structured = if needs_dispatch {
        false
    } else if let Some(joins) = postdominators(&writer.graph) {
        let nodes = proto.blocks.len();
        writer.region(0, nodes, depth, &joins, &mut (8 * nodes + 8))?
    } else if let Some(shape) = simple_loop(&writer.graph) {
        writer.natural_loop(shape, depth)?
    } else {
        false
    };
    if !structured {
        writer.text.truncate(mark);
        writer.dispatch(depth)?;
    }
    if linked {
        writer.line(1, "end");
        writer.line(0, "end");
    } else if depth > 0 {
        writer.line(0, "end");
        writer.line(0, &format!("return {}_entry(...)", writer.prefix));
    }
    Ok(writer.text)
}
