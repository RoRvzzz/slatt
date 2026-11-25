use full_moon::{ast, node::Node, visitors::Visitor};
use rhai::{Array, Dynamic, Engine, EvalAltResult, Map};
use std::rc::Rc;

#[derive(Default)]
struct Walk {
    nodes: Vec<SyntaxNode>,
    active: Vec<Option<usize>>,
}

struct SyntaxNode {
    kind: &'static str,
    begin: usize,
    end: usize,
    line: usize,
    column: usize,
    parent: Option<usize>,
    children: Vec<usize>,
}

impl Walk {
    fn enter(&mut self, kind: &'static str, node: &impl Node) {
        let Some((begin, end)) = node.range() else {
            self.active.push(None);
            return;
        };
        let id = self.nodes.len();
        let parent = self.active.iter().rev().find_map(|entry| *entry);
        if let Some(parent) = parent {
            self.nodes[parent].children.push(id);
        }
        self.nodes.push(SyntaxNode {
            kind,
            begin: begin.bytes(),
            end: end.bytes(),
            line: begin.line(),
            column: begin.character(),
            parent,
            children: Vec::new(),
        });
        self.active.push(Some(id));
    }
}

macro_rules! nodes {
    ($($begin:ident, $end:ident: $type:ty => $kind:literal;)+) => {$ (
        fn $begin(&mut self, node: &$type) { self.enter($kind, node); }
        fn $end(&mut self, _: &$type) { self.active.pop(); }
    )+ };
}

impl Visitor for Walk {
    nodes! {
        visit_block, visit_block_end: ast::Block => "block";
        visit_assignment, visit_assignment_end: ast::Assignment => "assignment";
        visit_local_assignment, visit_local_assignment_end: ast::LocalAssignment => "local_assignment";
        visit_function_declaration, visit_function_declaration_end: ast::FunctionDeclaration => "function_declaration";
        visit_local_function, visit_local_function_end: ast::LocalFunction => "local_function";
        visit_anonymous_function, visit_anonymous_function_end: ast::AnonymousFunction => "anonymous_function";
        visit_function_body, visit_function_body_end: ast::FunctionBody => "function_body";
        visit_function_call, visit_function_call_end: ast::FunctionCall => "function_call";
        visit_method_call, visit_method_call_end: ast::MethodCall => "method_call";
        visit_function_args, visit_function_args_end: ast::FunctionArgs => "arguments";
        visit_parameter, visit_parameter_end: ast::Parameter => "parameter";
        visit_do, visit_do_end: ast::Do => "do";
        visit_if, visit_if_end: ast::If => "if";
        visit_else_if, visit_else_if_end: ast::ElseIf => "elseif";
        visit_while, visit_while_end: ast::While => "while";
        visit_repeat, visit_repeat_end: ast::Repeat => "repeat";
        visit_numeric_for, visit_numeric_for_end: ast::NumericFor => "numeric_for";
        visit_generic_for, visit_generic_for_end: ast::GenericFor => "generic_for";
        visit_return, visit_return_end: ast::Return => "return";
        visit_index, visit_index_end: ast::Index => "index";
        visit_table_constructor, visit_table_constructor_end: ast::TableConstructor => "table";
        visit_field, visit_field_end: ast::Field => "field";
        visit_compound_assignment, visit_compound_assignment_end: ast::CompoundAssignment => "compound_assignment";
        visit_if_expression, visit_if_expression_end: ast::luau::IfExpression => "if_expression";
    }

    fn visit_expression(&mut self, node: &ast::Expression) {
        let kind = match node {
            ast::Expression::BinaryOperator { .. } => "binary",
            ast::Expression::UnaryOperator { .. } => "unary",
            ast::Expression::String(_) => "string",
            ast::Expression::Number(_) => "number",
            ast::Expression::Symbol(_) => "symbol",
            ast::Expression::Var(_) => "variable",
            _ => "expression",
        };
        self.enter(kind, node);
    }

    fn visit_expression_end(&mut self, _: &ast::Expression) {
        self.active.pop();
    }
}

#[derive(Clone)]
struct LuaAst(Rc<[SyntaxNode]>);

fn parse(source: &str) -> Result<LuaAst, Box<EvalAltResult>> {
    let ast = full_moon::parse(source).map_err(|errors| errors[0].to_string())?;
    let mut walk = Walk::default();
    walk.visit_ast(&ast);
    Ok(LuaAst(walk.nodes.into()))
}

fn node_map(id: usize, node: &SyntaxNode) -> Map {
    Map::from([
        ("id".into(), Dynamic::from(id as i64)),
        ("kind".into(), node.kind.into()),
        ("begin".into(), Dynamic::from(node.begin as i64)),
        ("end".into(), Dynamic::from(node.end as i64)),
        ("line".into(), Dynamic::from(node.line as i64)),
        ("column".into(), Dynamic::from(node.column as i64)),
        (
            "parent".into(),
            node.parent.map_or(Dynamic::UNIT, |n| (n as i64).into()),
        ),
    ])
}

pub fn register(engine: &mut Engine) {
    engine.register_type_with_name::<LuaAst>("LuaAst");
    engine.register_fn("lua_ast", parse);
    engine.register_get("len", |ast: &mut LuaAst| ast.0.len() as i64);
    engine.register_fn("nodes", |ast: &mut LuaAst, kind: &str| -> Array {
        ast.0
            .iter()
            .enumerate()
            .filter(|(_, node)| kind.is_empty() || node.kind == kind)
            .map(|(id, node)| Dynamic::from_map(node_map(id, node)))
            .collect()
    });
    engine.register_fn(
        "node",
        |ast: &mut LuaAst, id: i64| -> Result<Map, Box<EvalAltResult>> {
            let node = ast
                .0
                .get(id as usize)
                .ok_or("Lua AST node is out of bounds")?;
            Ok(node_map(id as usize, node))
        },
    );
    engine.register_fn(
        "children",
        |ast: &mut LuaAst, id: i64| -> Result<Array, Box<EvalAltResult>> {
            let node = ast
                .0
                .get(id as usize)
                .ok_or("Lua AST node is out of bounds")?;
            Ok(node
                .children
                .iter()
                .map(|&id| Dynamic::from_map(node_map(id, &ast.0[id])))
                .collect())
        },
    );
    engine.register_fn(
        "enclosing",
        |ast: &mut LuaAst, kind: &str, begin: i64, end: i64| -> Result<Map, Box<EvalAltResult>> {
            let begin = usize::try_from(begin).map_err(|_| "invalid Lua AST span")?;
            let end = usize::try_from(end).map_err(|_| "invalid Lua AST span")?;
            if begin >= end {
                return Err("Lua AST span must be nonempty".into());
            }
            let (id, node) = ast
                .0
                .iter()
                .enumerate()
                .filter(|(_, node)| node.kind == kind && node.begin <= begin && end <= node.end)
                .min_by_key(|(_, node)| node.end - node.begin)
                .ok_or_else(|| format!("no enclosing Lua AST node of kind {kind:?}"))?;
            Ok(node_map(id, node))
        },
    );
}
