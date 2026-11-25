use full_moon::{
    ast::{BinOp, Block, Expression, If, Stmt, UnOp, Var},
    node::Node,
    tokenizer::{Symbol, TokenType},
    visitors::Visitor,
};
use rhai::{Array, Dynamic, Engine, EvalAltResult, Map};

const MAX_EXACT: i64 = 1 << 53;

fn integer(expr: &Expression, selector: &str, opcode: i64) -> Option<i64> {
    let value = match expr {
        Expression::Parentheses { expression, .. } => integer(expression, selector, opcode)?,
        Expression::Var(Var::Name(name)) if matches!(name.token().token_type(), TokenType::Identifier { identifier } if identifier.as_str() == selector) => {
            opcode
        }
        Expression::Number(number) => {
            let text = number.token().to_string().replace('_', "");
            if let Some(hex) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
                i64::from_str_radix(hex, 16).ok()?
            } else {
                text.parse::<i64>().ok()?
            }
        }
        Expression::UnaryOperator {
            unop: UnOp::Minus(_),
            expression,
        } => integer(expression, selector, opcode)?.checked_neg()?,
        Expression::BinaryOperator { lhs, binop, rhs } => {
            let a = integer(lhs, selector, opcode)?;
            let b = integer(rhs, selector, opcode)?;
            match binop {
                BinOp::Plus(_) => a.checked_add(b)?,
                BinOp::Minus(_) => a.checked_sub(b)?,
                BinOp::Star(_) => a.checked_mul(b)?,
                _ => return None,
            }
        }
        _ => return None,
    };
    (-MAX_EXACT..=MAX_EXACT).contains(&value).then_some(value)
}

fn predicate(expr: &Expression, selector: &str, opcode: i64) -> Option<bool> {
    Some(match expr {
        Expression::Parentheses { expression, .. } => predicate(expression, selector, opcode)?,
        Expression::Symbol(symbol) => match symbol.token().token_type() {
            TokenType::Symbol {
                symbol: Symbol::True,
            } => true,
            TokenType::Symbol {
                symbol: Symbol::False | Symbol::Nil,
            } => false,
            _ => return None,
        },
        Expression::UnaryOperator {
            unop: UnOp::Not(_),
            expression,
        } => !predicate(expression, selector, opcode)?,
        Expression::BinaryOperator { lhs, binop, rhs } => match binop {
            BinOp::And(_) => predicate(lhs, selector, opcode)? && predicate(rhs, selector, opcode)?,
            BinOp::Or(_) => predicate(lhs, selector, opcode)? || predicate(rhs, selector, opcode)?,
            _ => {
                let a = integer(lhs, selector, opcode)?;
                let b = integer(rhs, selector, opcode)?;
                match binop {
                    BinOp::TwoEqual(_) => a == b,
                    BinOp::TildeEqual(_) => a != b,
                    BinOp::LessThan(_) => a < b,
                    BinOp::LessThanEqual(_) => a <= b,
                    BinOp::GreaterThan(_) => a > b,
                    BinOp::GreaterThanEqual(_) => a >= b,
                    _ => return None,
                }
            }
        },
        _ => return None,
    })
}

fn branch<'a>(node: &'a If, selector: &str, opcode: i64) -> Result<Option<&'a Block>, ()> {
    if node.binding().is_some() {
        return Err(());
    }
    if predicate(node.condition(), selector, opcode).ok_or(())? {
        return Ok(Some(node.block()));
    }
    for clause in node.else_if().into_iter().flatten() {
        if clause.binding().is_some() {
            return Err(());
        }
        if predicate(clause.condition(), selector, opcode).ok_or(())? {
            return Ok(Some(clause.block()));
        }
    }
    Ok(node.else_block())
}

fn handler<'a>(node: &'a If, selector: &str, opcode: i64) -> Result<Option<&'a Block>, ()> {
    let mut region = branch(node, selector, opcode)?;
    while let Some(block) = region {
        let mut statements = block.stmts();
        let Some(Stmt::If(nested)) = statements.next() else {
            break;
        };
        if statements.next().is_some() || block.last_stmt().is_some() {
            break;
        }
        let Ok(selected) = branch(nested, selector, opcode) else {
            break;
        };
        region = selected;
    }
    Ok(region)
}

struct FindIf {
    offset: usize,
    found: Option<If>,
}

impl Visitor for FindIf {
    fn visit_if(&mut self, node: &If) {
        if node.if_token().token().start_position().bytes() == self.offset {
            self.found = Some(node.clone());
        }
    }
}

fn dispatch(
    source: &str,
    begin: i64,
    selector: &str,
    opcodes: Array,
) -> Result<Array, Box<EvalAltResult>> {
    let offset = usize::try_from(begin).map_err(|_| "invalid dispatcher start offset")?;
    if opcodes.is_empty() || opcodes.len() > 4_096 {
        return Err("dispatcher needs 1–4,096 candidate opcodes".into());
    }
    let ast = full_moon::parse(source).map_err(|errors| errors[0].to_string())?;
    let mut lookup = FindIf {
        offset,
        found: None,
    };
    lookup.visit_ast(&ast);
    let node = lookup
        .found
        .ok_or_else(|| format!("no Lua if statement starts at byte {offset}"))?;
    opcodes
        .into_iter()
        .map(|opcode| {
            let opcode = opcode
                .as_int()
                .map_err(|_| "dispatcher opcodes must be integers")?;
            if !(-MAX_EXACT..=MAX_EXACT).contains(&opcode) {
                return Err("dispatcher opcode is outside the exact integer range".into());
            }
            let selected = handler(&node, selector, opcode)
                .map_err(|()| format!("unsupported dispatcher condition for opcode {opcode}"))?;
            let (start, end, text) = selected.and_then(Node::range).map_or(
                (Dynamic::UNIT, Dynamic::UNIT, ""),
                |(start, end)| {
                    (
                        Dynamic::from(start.bytes() as i64),
                        Dynamic::from(end.bytes() as i64),
                        &source[start.bytes()..end.bytes()],
                    )
                },
            );
            Ok(Dynamic::from_map(Map::from([
                ("opcode".into(), opcode.into()),
                ("begin".into(), start),
                ("end".into(), end),
                ("text".into(), text.into()),
            ])))
        })
        .collect()
}

pub fn register(engine: &mut Engine) {
    engine.register_fn("lua_dispatch", dispatch);
}
