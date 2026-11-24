use crate::{parser, rules::Script};
use full_moon::{node::Node, tokenizer::TokenType};
use rhai::{AST, Array, Dynamic, Engine, EvalAltResult, Scope};
use sha2::{Digest, Sha256};

fn lua_strings(source: &str) -> Result<Array, Box<EvalAltResult>> {
    let ast = full_moon::parse(source).map_err(|errors| errors[0].to_string())?;
    Ok(ast
        .nodes()
        .tokens()
        .filter_map(|token| {
            if let TokenType::StringLiteral { literal, .. } = token.token().token_type() {
                Some(Dynamic::from(literal.to_string()))
            } else {
                None
            }
        })
        .collect())
}

fn lua_token_text(source: &str) -> Result<Vec<String>, Box<EvalAltResult>> {
    let ast = full_moon::parse(source).map_err(|errors| errors[0].to_string())?;
    let mut tokens: Vec<_> = ast.nodes().tokens().collect();
    tokens.sort_unstable_by_key(|token| token.token().start_position().bytes());
    Ok(tokens
        .iter()
        .map(|token| token.token().to_string())
        .collect())
}

fn engine() -> Engine {
    let mut engine = Engine::new();
    engine.set_max_operations(2_000_000);
    engine.set_max_string_size(1 << 20);
    engine.set_max_array_size(65_536);
    engine.set_max_expr_depths(128, 128);
    engine.register_fn("lua_strings", lua_strings);
    engine.register_fn(
        "lua_tokens",
        |source: &str| -> Result<String, Box<EvalAltResult>> {
            Ok(lua_token_text(source)?.join(" "))
        },
    );
    engine.register_fn(
        "lua_token_list",
        |source: &str| -> Result<Array, Box<EvalAltResult>> {
            Ok(lua_token_text(source)?
                .into_iter()
                .map(Dynamic::from)
                .collect())
        },
    );
    engine.register_fn("bytes", |s: &str| s.as_bytes().to_vec());
    engine.register_fn("sha256", |s: &str| format!("{:x}", Sha256::digest(s)));
    engine.register_fn("sha256", |data: rhai::Blob| {
        format!("{:x}", Sha256::digest(data))
    });
    engine.register_fn("byte_char", |n: i64| char::from(n as u8));
    engine
}

pub fn parse(text: &str) -> Result<(Script, Option<AST>), String> {
    let text = text.replace("\r\n", "\n");
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    let leading: usize = text
        .split_inclusive('\n')
        .take_while(|line| line.trim().is_empty() || line.trim_start().starts_with("//"))
        .map(str::len)
        .sum();
    if let Some(frontend) = text[leading..].strip_prefix("frontend\n") {
        let (source, passes) = frontend
            .split_once("\n%%\n")
            .or_else(|| frontend.strip_suffix("\n%%").map(|source| (source, "")))
            .ok_or("expected %% after source frontend")?;
        let header = text[..leading].lines().count() + 1;
        let ast = engine()
            .compile("\n".repeat(header) + source)
            .map_err(|error| {
                let pos = error.position();
                format!(
                    "{}:{}: {}",
                    pos.line().unwrap_or(header + 1),
                    pos.position().unwrap_or(1),
                    error.err_type()
                )
            })?;
        let rule_padding = header + source.bytes().filter(|byte| *byte == b'\n').count() + 2;
        Ok((
            parser::parse_script(&("\n".repeat(rule_padding) + passes))?,
            Some(ast),
        ))
    } else {
        Ok((parser::parse_script(text)?, None))
    }
}

pub fn lower(frontend: &AST, source: &str, source_output: bool) -> Result<Dynamic, String> {
    if source.len() > 1024 * 1024 {
        return Err("source frontend input exceeds 1 MiB".into());
    }
    let mut scope = Scope::new();
    scope.push_constant("source", source.trim_start_matches('\u{feff}').to_owned());
    scope.push_constant("source_output", source_output);
    let mut engine = engine();
    // Source decoders need budget for each byte they unpack and parse.
    engine.set_max_operations((2_000_000 + 64 * source.len() as u64).min(16_000_000));
    engine
        .eval_ast_with_scope::<Dynamic>(&mut scope, frontend)
        .map_err(|error| error.to_string())
}
