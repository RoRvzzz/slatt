use crate::{parser, rules::Script};
use full_moon::{
    node::Node,
    tokenizer::{StringLiteralQuoteType, TokenType},
};
use rhai::{AST, Array, Dynamic, Engine, EvalAltResult, ImmutableString, Map, Scope};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::rc::Rc;

pub const MAX_INPUT_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Copy)]
pub enum Output {
    Ir,
    Source,
    Inspect,
    Bytes,
}

impl Output {
    fn name(self) -> &'static str {
        match self {
            Self::Ir => "ir",
            Self::Source => "source",
            Self::Inspect => "inspect",
            Self::Bytes => "bytes",
        }
    }
}

#[derive(Clone)]
struct LuaTokenBuffer {
    tokens: Rc<[ImmutableString]>,
    spans: Rc<[(usize, usize)]>,
}

fn token_buffer(source: &str) -> Result<LuaTokenBuffer, Box<EvalAltResult>> {
    let ast = full_moon::parse(source).map_err(|errors| errors[0].to_string())?;
    let mut tokens: Vec<_> = ast.nodes().tokens().collect();
    tokens.sort_unstable_by_key(|token| token.token().start_position().bytes());
    Ok(LuaTokenBuffer {
        tokens: tokens
            .iter()
            .map(|token| ImmutableString::from(token.token().to_string()))
            .collect(),
        spans: tokens
            .iter()
            .map(|token| {
                (
                    token.token().start_position().bytes(),
                    token.token().end_position().bytes(),
                )
            })
            .collect(),
    })
}

impl LuaTokenBuffer {
    fn find(&mut self, value: &str, start: i64) -> i64 {
        let start = start.max(0) as usize;
        self.tokens
            .iter()
            .skip(start)
            .position(|token| token.as_str() == value)
            .map_or(-1, |offset| (start + offset) as i64)
    }

    fn range(&self, begin: i64, end: i64) -> Result<&[ImmutableString], Box<EvalAltResult>> {
        self.tokens
            .get(begin as usize..end as usize)
            .ok_or_else(|| "Lua token range is out of bounds".into())
    }

    fn find_tokens(&mut self, pattern: Array, start: i64) -> Result<i64, Box<EvalAltResult>> {
        if pattern.is_empty() {
            return Err("Lua token pattern must be nonempty".into());
        }
        let pattern = pattern
            .into_iter()
            .map(|value| {
                value
                    .try_cast::<ImmutableString>()
                    .ok_or("Lua token pattern needs strings")
            })
            .collect::<Result<Vec<_>, _>>()?;
        let start = start.max(0) as usize;
        Ok(self
            .tokens
            .get(start..)
            .unwrap_or_default()
            .windows(pattern.len())
            .position(|tokens| tokens == pattern.as_slice())
            .map_or(-1, |offset| (start + offset) as i64))
    }

    fn match_pattern(&mut self, pattern: Self) -> Dynamic {
        Self::bindings(&self.tokens, &pattern.tokens).map_or(Dynamic::UNIT, Dynamic::from_map)
    }

    fn match_span(
        &mut self,
        begin: i64,
        end: i64,
        patterns: Array,
    ) -> Result<Dynamic, Box<EvalAltResult>> {
        let invalid_span =
            || -> Box<EvalAltResult> { "Lua match span must cover complete tokens".into() };
        let begin = usize::try_from(begin).map_err(|_| invalid_span())?;
        let end = usize::try_from(end).map_err(|_| invalid_span())?;
        let first = self
            .spans
            .binary_search_by_key(&begin, |span| span.0)
            .map_err(|_| invalid_span())?;
        let last = self.spans.partition_point(|span| span.0 < end);
        if first >= last || self.spans[last - 1].1 != end {
            return Err(invalid_span());
        }
        let patterns = patterns
            .into_iter()
            .map(|value| {
                value
                    .try_cast::<Self>()
                    .ok_or("Lua match span pattern list needs token buffers")
            })
            .collect::<Result<Vec<_>, _>>()?;
        let actual = &self.tokens[first..last];
        for (index, pattern) in patterns.iter().enumerate() {
            if let Some(bindings) = Self::bindings(actual, &pattern.tokens) {
                return Ok(Dynamic::from_array(vec![
                    Dynamic::from(index as i64),
                    Dynamic::from_map(bindings),
                ]));
            }
        }
        Ok(Dynamic::UNIT)
    }

    fn bindings(actual: &[ImmutableString], expected: &[ImmutableString]) -> Option<Map> {
        fn trim(tokens: &[ImmutableString]) -> &[ImmutableString] {
            let end = tokens.iter().rposition(|token| token.as_str() != ";");
            &tokens[..end.map_or(0, |index| index + 1)]
        }

        let actual = trim(actual);
        let expected = trim(expected);
        if actual.is_empty() || actual.len() != expected.len() {
            return None;
        }
        let mut captures = BTreeMap::new();
        for (actual, expected) in actual.iter().zip(expected) {
            if let Some(name) = expected
                .strip_prefix("__match_")
                .filter(|name| !name.is_empty())
            {
                if captures
                    .insert(name, actual)
                    .is_some_and(|previous| previous != actual)
                {
                    return None;
                }
            } else if actual != expected {
                return None;
            }
        }
        Some(
            captures
                .into_iter()
                .map(|(name, token)| (name.into(), token.clone().into()))
                .collect(),
        )
    }
}

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

fn lua_long_strings(source: &str) -> Result<Array, Box<EvalAltResult>> {
    let ast = full_moon::parse(source).map_err(|errors| errors[0].to_string())?;
    Ok(ast
        .nodes()
        .tokens()
        .filter_map(|token| {
            let token = token.token();
            let TokenType::StringLiteral {
                literal,
                quote_type: StringLiteralQuoteType::Brackets,
                ..
            } = token.token_type()
            else {
                return None;
            };
            Some(Dynamic::from_map(Map::from([
                ("text".into(), literal.to_string().into()),
                (
                    "begin".into(),
                    (token.start_position().bytes() as i64).into(),
                ),
                ("end".into(), (token.end_position().bytes() as i64).into()),
            ])))
        })
        .collect())
}

fn lua_token_text(source: &str) -> Result<Vec<String>, Box<EvalAltResult>> {
    Ok(token_buffer(source)?
        .tokens
        .iter()
        .map(ToString::to_string)
        .collect())
}

fn engine() -> Engine {
    let mut engine = Engine::new();
    crate::cursor::register(&mut engine);
    crate::codecs::register(&mut engine);
    crate::lua_ast::register(&mut engine);
    crate::lua_dispatch::register(&mut engine);
    crate::frontend_ir::register(&mut engine);
    engine.register_fn("lua_quote", |bytes: &mut rhai::Blob| {
        crate::lua_emit::lua_string(bytes)
    });
    engine.register_fn("lua_quote", |text: &str| {
        crate::lua_emit::lua_string(text.as_bytes())
    });
    engine.register_fn(
        "luau_trace",
        |source: &str, fixture: &str, name: &str| -> Result<Dynamic, Box<EvalAltResult>> {
            let report = crate::luau_runtime::trace(source, fixture, name)?;
            rhai::serde::to_dynamic(report)
        },
    );
    engine.register_fn(
        "luau_capture",
        |source: &str, fixture: &str, name: &str| -> Result<Map, Box<EvalAltResult>> {
            let captured = crate::luau_runtime::capture(source, fixture, name)?;
            let artifacts: Map = captured
                .artifacts
                .into_iter()
                .map(|(label, bytes)| (label.into(), Dynamic::from_blob(bytes)))
                .collect();
            Ok(Map::from([
                ("report".into(), rhai::serde::to_dynamic(captured.report)?),
                ("artifacts".into(), Dynamic::from_map(artifacts)),
            ]))
        },
    );
    engine.register_fn(
        "luau_replay",
        |original: &str, recovered: &str, fixture: &str| -> Result<Dynamic, Box<EvalAltResult>> {
            rhai::serde::to_dynamic(crate::luau_runtime::replay(original, recovered, fixture)?)
        },
    );
    engine.register_fn(
        "luau_capture_expr",
        |source: &str,
         begin: i64,
         end: i64,
         fixture: &str,
         label: &str|
         -> Result<rhai::Blob, Box<EvalAltResult>> {
            Ok(crate::luau_runtime::capture_expression(
                source,
                begin,
                end,
                fixture,
                label,
                crate::luau_runtime::DEFAULT_TIMEOUT_MS,
            )?)
        },
    );
    engine.register_fn(
        "luau_capture_expr",
        |source: &str,
         begin: i64,
         end: i64,
         fixture: &str,
         label: &str,
         timeout_ms: i64|
         -> Result<rhai::Blob, Box<EvalAltResult>> {
            Ok(crate::luau_runtime::capture_expression(
                source, begin, end, fixture, label, timeout_ms,
            )?)
        },
    );
    engine.register_fn(
        "replace_once",
        |source: &str, needle: &str, replacement: &str| -> Result<String, Box<EvalAltResult>> {
            let first = source.find(needle);
            if needle.is_empty() || first.is_none() || first != source.rfind(needle) {
                return Err("replace_once requires one nonempty matching anchor".into());
            }
            Ok(source.replacen(needle, replacement, 1))
        },
    );
    engine.register_fn(
        "slice_bytes",
        |source: &str, begin: i64, end: i64| -> Result<String, Box<EvalAltResult>> {
            source
                .get(begin as usize..end as usize)
                .map(str::to_owned)
                .ok_or_else(|| "invalid source byte span".into())
        },
    );
    engine.set_max_operations(2_000_000);
    engine.set_max_string_size(1 << 20);
    engine.set_max_array_size(65_536);
    engine.set_max_expr_depths(128, 128);
    engine.register_fn("lua_strings", lua_strings);
    engine.register_fn("lua_long_strings", lua_long_strings);
    engine.register_fn(
        "lua_number_from_bits",
        |bits: i64, encoding: &str| -> Result<f64, Box<EvalAltResult>> {
            Ok(match encoding {
                "i64" => bits as f64,
                "u64" => bits as u64 as f64,
                "f64" => f64::from_bits(bits as u64),
                _ => return Err("Lua number encoding must be i64, u64 or f64".into()),
            })
        },
    );
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
    engine.register_type_with_name::<LuaTokenBuffer>("LuaTokenBuffer");
    engine.register_fn("lua_token_buffer", token_buffer);
    engine.register_get("len", |tokens: &mut LuaTokenBuffer| {
        tokens.tokens.len() as i64
    });
    engine.register_indexer_get(|tokens: &mut LuaTokenBuffer, index: i64| {
        tokens
            .tokens
            .get(index as usize)
            .cloned()
            .ok_or_else(|| Box::<EvalAltResult>::from("Lua token index is out of bounds"))
    });
    engine.register_fn(
        "token_span",
        |tokens: &mut LuaTokenBuffer, index: i64| -> Result<Array, Box<EvalAltResult>> {
            let (begin, end) = tokens
                .spans
                .get(index as usize)
                .ok_or("Lua token index is out of bounds")?;
            Ok(vec![(*begin as i64).into(), (*end as i64).into()])
        },
    );
    engine.register_fn(
        "splice_bytes",
        |source: &str,
         begin: i64,
         end: i64,
         replacement: &str|
         -> Result<String, Box<EvalAltResult>> {
            if source.get(begin as usize..end as usize).is_none() {
                return Err("invalid Lua source byte span".into());
            }
            let mut output = source.to_owned();
            output.replace_range(begin as usize..end as usize, replacement);
            Ok(output)
        },
    );
    engine.register_fn("index_of", LuaTokenBuffer::find);
    engine.register_fn("find_tokens", LuaTokenBuffer::find_tokens);
    engine.register_fn("match_pattern", LuaTokenBuffer::match_pattern);
    engine.register_fn("match_span", LuaTokenBuffer::match_span);
    engine.register_fn("contains", |tokens: &mut LuaTokenBuffer, value: &str| {
        tokens.find(value, 0) >= 0
    });
    engine.register_fn(
        "extract",
        |tokens: &mut LuaTokenBuffer,
         begin: i64,
         count: i64|
         -> Result<Array, Box<EvalAltResult>> {
            Ok(tokens
                .range(begin, begin.saturating_add(count))?
                .iter()
                .cloned()
                .map(Dynamic::from)
                .collect())
        },
    );
    engine.register_fn(
        "join_lua_tokens",
        |tokens: &mut LuaTokenBuffer, begin: i64, end: i64| -> Result<String, Box<EvalAltResult>> {
            let mut text = String::new();
            for token in tokens.range(begin, end)? {
                text.push_str(token);
                text.push(' ');
            }
            Ok(text)
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

#[cfg(test)]
pub fn lower(frontend: &AST, source: &str, source_output: bool) -> Result<Dynamic, String> {
    let mode = if source_output {
        Output::Source
    } else {
        Output::Ir
    };
    evaluate(frontend, source.as_bytes().to_vec(), mode, "")
}

pub fn evaluate(
    frontend: &AST,
    input: Vec<u8>,
    output: Output,
    host_fixture: &str,
) -> Result<Dynamic, String> {
    let len = input.len();
    if len > MAX_INPUT_BYTES {
        return Err("source frontend input exceeds 4 MiB".into());
    }
    let source = std::str::from_utf8(&input)
        .ok()
        .map(|text| Dynamic::from(text.trim_start_matches('\u{feff}').to_owned()))
        .unwrap_or(Dynamic::UNIT);
    let mut scope = Scope::new();
    scope.push_constant_dynamic("source", source);
    scope.push_constant_dynamic("input_bytes", Dynamic::from_blob(input));
    scope.push_constant("source_output", matches!(output, Output::Source));
    scope.push_constant("output_mode", output.name());
    scope.push_constant("host_fixture", host_fixture.to_owned());
    let mut engine = engine();
    // Token arrays and rendered source scale with the size of the input.
    let array_limit = if matches!(output, Output::Inspect | Output::Bytes) {
        MAX_INPUT_BYTES
    } else {
        len.max(65_536)
    };
    engine.set_max_array_size(array_limit);
    engine.set_max_string_size((3 * len).max(1 << 20));
    engine.set_max_operations(2_000_000 + 64 * len as u64);
    engine
        .eval_ast_with_scope::<Dynamic>(&mut scope, frontend)
        .map_err(|error| error.to_string())
}
