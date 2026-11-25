mod binary;
mod codecs;
mod cursor;
mod emit;
mod frontend;
mod frontend_ir;
mod ir;
mod lua_ast;
mod lua_dispatch;
mod lua_emit;
mod luau_runtime;
mod parser;
mod project;
mod rules;

use ir::{Facts, Proto};
use rules::Script;
use std::env;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::Path;

#[cfg(test)]
mod tests;

const HELP: &str = "Slatt — standalone bytecode pass language

Usage:
  slatt check <rules.slatt | ->
  slatt check - --stdin-path <rules.slatt>
  slatt import <bytecode>
  slatt run <rules.slatt> <input>
  slatt recover <rules.slatt> <source> -o <output.lua>
  slatt unpack <rules.slatt> [<input>] -o <artifact>
  slatt trace <source> [--fixture <fixture.luau>]
  slatt replay <original.luau> <recovered.luau> [--fixture <fixture.luau>]
  slatt inspect <rules.slatt> <source> [--fixture <fixture.luau>]
  slatt run <rules.slatt> <input> --emit <lua|lua51|luau> -o <output>
  slatt emit <lua|lua51|luau> <input> <output>
  slatt facts <input> <proto> <block> <instruction>

run and facts accept .pfir, Lua 5.1 chunks, or Luau bytecode versions 6–14.
run also accepts source text when the rules declare a frontend.
Rules can include other files with include \"relative/path.slatt\";.
import and run print normalized IR by default. Binary output creates a new file.
facts reports known constants
before a zero-based instruction within the named block.
";

fn file<T>(path: &str, parse: impl FnOnce(&str) -> Result<T, String>) -> Result<T, String> {
    let text = if path == "-" {
        let mut text = String::new();
        io::stdin()
            .read_to_string(&mut text)
            .map_err(|err| format!("stdin: {err}"))?;
        text
    } else {
        fs::read_to_string(path).map_err(|err| format!("{path}: {err}"))?
    };
    parse(&text).map_err(|err| format!("{path}:{err}"))
}

fn frontend_result(
    frontend: &rhai::AST,
    path: &str,
    mode: frontend::Output,
    fixture: &str,
) -> Result<rhai::Dynamic, String> {
    let limit = frontend::MAX_INPUT_BYTES as u64 + 1;
    let mut input = Vec::new();
    if path == "-" {
        io::stdin().lock().take(limit).read_to_end(&mut input)
    } else {
        fs::File::open(path).and_then(|file| file.take(limit).read_to_end(&mut input))
    }
    .map_err(|error| format!("{path}: {error}"))?;
    frontend::evaluate(frontend, input, mode, fixture).map_err(|error| format!("{path}:{error}"))
}

fn program(path: &str, script: &Script, binary_only: bool) -> Result<Vec<Proto>, String> {
    let bytes = fs::read(path).map_err(|err| format!("{path}: {err}"))?;
    let binary_extension = Path::new(path)
        .extension()
        .and_then(|part| part.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("luac") || ext.eq_ignore_ascii_case("luauc"));
    let binary_prefix = bytes.starts_with(b"\x1bLua")
        || bytes
            .first()
            .is_some_and(|byte| *byte <= 14 && (!byte.is_ascii_whitespace() || bytes.contains(&0)));
    let result = if binary_only || binary_extension || binary_prefix {
        binary::import(&bytes)
    } else {
        std::str::from_utf8(&bytes)
            .map_err(|err| err.to_string())
            .and_then(|text| parser::parse_ir(text, script))
    };
    result.map_err(|error| format!("{path}: {error}"))
}

fn print_program(mut output: impl Write, protos: &[Proto]) -> io::Result<()> {
    for (index, proto) in protos.iter().enumerate() {
        if index != 0 {
            writeln!(output)?;
        }
        write!(output, "{proto}")?;
    }
    Ok(())
}

fn run_passes(rules: &str, input: &str) -> Result<Vec<Proto>, String> {
    let (script, source_frontend) = rule_file(rules, None)?;
    let mut protos = if let Some(source_frontend) = source_frontend {
        let ir = frontend_result(&source_frontend, input, frontend::Output::Ir, "")?;
        if ir.is::<frontend_ir::Lifted>() {
            vec![ir.cast::<frontend_ir::Lifted>().into_proto()]
        } else if ir.is::<rhai::Array>() {
            frontend_ir::program(ir.cast::<rhai::Array>())?
        } else if ir.is::<rhai::Blob>() {
            binary::import(&ir.cast::<rhai::Blob>())?
        } else {
            let ir = ir.try_cast::<rhai::ImmutableString>().ok_or(
                "source frontend must return a PFIR prototype, PFIR text, or bytecode bytes",
            )?;
            parser::parse_ir(&ir, &script)?
        }
    } else {
        program(input, &script, false)?
    };
    apply_passes(&script, &mut protos)?;
    Ok(protos)
}

fn apply_passes(script: &Script, protos: &mut [Proto]) -> Result<(), String> {
    for proto in protos {
        for pass in &script.passes {
            pass.run(proto)?;
        }
    }
    Ok(())
}

fn rule_file(path: &str, stdin_path: Option<&str>) -> Result<(Script, Option<rhai::AST>), String> {
    let text = file(path, |source| Ok(source.to_owned()))?;
    project::load(&text, Path::new(stdin_path.unwrap_or(path)), path)
}

fn check_rules(path: &str, stdin_path: Option<&str>, mut output: impl Write) -> Result<(), String> {
    let (script, _) = rule_file(path, stdin_path)?;
    writeln!(
        output,
        "OK: {} pass(es), {} opcode map(s)",
        script.passes.len(),
        script.maps.len()
    )
    .map_err(|error| error.to_string())
}

fn write_program(format: &str, protos: &[Proto], path: &str) -> Result<(), String> {
    if format == "lua" {
        return write_lua(path, &lua_emit::emit_program(protos)?);
    }
    let dialect = emit::Dialect::parse(format)?;
    let bytes = emit::emit(dialect, protos)?;
    write_new(path, &bytes)
}

fn write_lua(path: &str, source: &str) -> Result<(), String> {
    if source.trim().is_empty() {
        return Err("frontend returned empty Lua source".into());
    }
    full_moon::parse(source).map_err(|errors| errors[0].to_string())?;
    write_new(path, source.as_bytes())
}

fn write_new(path: &str, bytes: &[u8]) -> Result<(), String> {
    let mut out = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|err| format!("{path}: {err}"))?;
    out.write_all(bytes).map_err(|err| format!("{path}: {err}"))
}

fn facts(proto: &Proto, block: &str, instruction: usize) -> Result<Option<Facts>, String> {
    let block = block.strip_prefix('@').unwrap_or(block);
    let index = proto
        .blocks
        .iter()
        .position(|entry| entry.name == block)
        .ok_or_else(|| format!("no block @{block} in prototype {}", proto.name))?;
    if instruction >= proto.blocks[index].instructions.len() {
        return Err(format!("instruction {instruction} is outside @{block}"));
    }
    Ok(proto.facts()?.swap_remove(index).swap_remove(instruction))
}

fn trace_luau(input: &str, fixture: Option<&str>, mut output: impl Write) -> Result<(), String> {
    let fixture = match fixture {
        Some(path) => file(path, |text| Ok(text.to_owned()))?,
        None => String::new(),
    };
    let report = file(input, |source| luau_runtime::trace(source, &fixture, input))?;
    print_json(&report, &mut output)
}

fn replay_luau(
    original: &str,
    recovered: &str,
    fixture: Option<&str>,
    output: impl Write,
) -> Result<(), String> {
    let original = file(original, |text| Ok(text.to_owned()))?;
    let recovered = file(recovered, |text| Ok(text.to_owned()))?;
    let fixture = fixture
        .map(|path| file(path, |text| Ok(text.to_owned())))
        .transpose()?
        .unwrap_or_default();
    let report = luau_runtime::replay(&original, &recovered, &fixture)?;
    print_json(&report, output)?;
    if report["matched"] == true {
        Ok(())
    } else {
        Err("Luau replay did not match".into())
    }
}

fn print_json(report: &serde_json::Value, mut output: impl Write) -> Result<(), String> {
    serde_json::to_writer_pretty(&mut output, &report).map_err(|error| error.to_string())?;
    writeln!(output).map_err(|error| error.to_string())
}

fn inspect_frontend(
    rules: &str,
    input: &str,
    fixture: Option<&str>,
    output: impl Write,
) -> Result<(), String> {
    let (_, frontend) = rule_file(rules, None)?;
    let frontend = frontend.ok_or("inspect requires a source frontend")?;
    let fixture = match fixture {
        Some(path) => file(path, |text| Ok(text.to_owned()))?,
        None => String::new(),
    };
    let result = frontend_result(&frontend, input, frontend::Output::Inspect, &fixture)?;
    let report = inspection_json(result)?;
    print_json(&report, output)
}

fn inspection_json(value: rhai::Dynamic) -> Result<serde_json::Value, String> {
    use serde_json::Value;
    if value.is::<rhai::Blob>() {
        return Ok(Value::Array(
            value
                .cast::<rhai::Blob>()
                .into_iter()
                .map(Value::from)
                .collect(),
        ));
    }
    if value.is::<rhai::Array>() {
        return value
            .cast::<rhai::Array>()
            .into_iter()
            .map(inspection_json)
            .collect::<Result<_, _>>()
            .map(Value::Array);
    }
    if value.is::<rhai::Map>() {
        return value
            .cast::<rhai::Map>()
            .into_iter()
            .map(|(key, value)| Ok((key.to_string(), inspection_json(value)?)))
            .collect::<Result<_, _>>()
            .map(Value::Object);
    }
    rhai::serde::from_dynamic(&value).map_err(|error| error.to_string())
}

fn unpack_frontend(rules: &str, input: Option<&str>, output: &str) -> Result<(), String> {
    let (_, frontend) = rule_file(rules, None)?;
    let frontend = frontend.ok_or("unpack requires a source frontend")?;
    let result = match input {
        Some(path) => frontend_result(&frontend, path, frontend::Output::Bytes, "")?,
        None => frontend::evaluate(&frontend, Vec::new(), frontend::Output::Bytes, "")?,
    };
    let bytes = result
        .try_cast::<rhai::Blob>()
        .ok_or("unpack requires a byte blob result")?;
    write_new(output, &bytes)
}

fn run(args: &[String], mut output: impl Write) -> Result<(), String> {
    let result = match args {
        [] => write!(output, "{HELP}"),
        [flag] if flag == "--help" || flag == "-h" => write!(output, "{HELP}"),
        [flag] if flag == "--version" => {
            writeln!(output, "slatt {}", env!("CARGO_PKG_VERSION"))
        }
        [verb, rule_path] if verb == "check" => {
            return check_rules(rule_path, None, output);
        }
        [verb, stdin, flag, path] if verb == "check" && stdin == "-" && flag == "--stdin-path" => {
            return check_rules(stdin, Some(path), output);
        }
        [verb, input_path] if verb == "import" => {
            let protos = program(input_path, &Script::default(), true)?;
            print_program(output, &protos)
        }
        [verb, rule_path, input_path] if verb == "run" => {
            let protos = run_passes(rule_path, input_path)?;
            print_program(output, &protos)
        }
        [verb, input] if verb == "trace" => return trace_luau(input, None, output),
        [verb, input, flag, fixture] if verb == "trace" && flag == "--fixture" => {
            return trace_luau(input, Some(fixture), output);
        }
        [verb, original, recovered] if verb == "replay" => {
            return replay_luau(original, recovered, None, output);
        }
        [verb, original, recovered, flag, fixture] if verb == "replay" && flag == "--fixture" => {
            return replay_luau(original, recovered, Some(fixture), output);
        }
        [verb, rules, input] if verb == "inspect" => {
            return inspect_frontend(rules, input, None, output);
        }
        [verb, rules, input, flag, fixture] if verb == "inspect" && flag == "--fixture" => {
            return inspect_frontend(rules, input, Some(fixture), output);
        }
        [verb, rules, input, flag, output] if verb == "unpack" && flag == "-o" => {
            return unpack_frontend(rules, Some(input), output);
        }
        [verb, rules, flag, output] if verb == "unpack" && flag == "-o" => {
            return unpack_frontend(rules, None, output);
        }
        [verb, rule_path, input_path, flag, output_path] if verb == "recover" && flag == "-o" => {
            let (script, source_frontend) = rule_file(rule_path, None)?;
            let source_frontend = source_frontend.ok_or("recover requires a source frontend")?;
            let recovered =
                frontend_result(&source_frontend, input_path, frontend::Output::Source, "")?;
            if recovered.is::<frontend_ir::Lifted>() || recovered.is::<rhai::Array>() {
                let mut protos = if recovered.is::<frontend_ir::Lifted>() {
                    vec![recovered.cast::<frontend_ir::Lifted>().into_proto()]
                } else {
                    frontend_ir::program(recovered.cast::<rhai::Array>())?
                };
                apply_passes(&script, &mut protos)?;
                return write_program("lua", &protos, output_path);
            }
            let recovered = recovered
                .try_cast::<rhai::ImmutableString>()
                .ok_or("recover requires a PFIR prototype or Lua source")?;
            return write_lua(output_path, &recovered);
        }
        [verb, rule_path, input_path, mode, format, flag, path]
            if verb == "run" && mode == "--emit" && flag == "-o" =>
        {
            return write_program(format, &run_passes(rule_path, input_path)?, path);
        }
        [verb, format, input_path, path] if verb == "emit" => {
            let protos = program(input_path, &Script::default(), false)?;
            return write_program(format, &protos, path);
        }
        [verb, input_path, proto_name, block, instruction] if verb == "facts" => {
            let instruction = instruction
                .parse::<usize>()
                .map_err(|_| "expected a zero-based instruction index")?;
            let protos = program(input_path, &Script::default(), false)?;
            let proto = protos
                .iter()
                .find(|proto| &proto.name == proto_name)
                .ok_or_else(|| format!("no prototype {proto_name}"))?;
            match facts(proto, block, instruction)? {
                None => writeln!(output, "unreachable"),
                Some(facts) if facts.is_empty() => writeln!(output, "no known constants"),
                Some(facts) => {
                    for (reg, value) in facts {
                        writeln!(output, "r{} = {value}", reg.0).map_err(|err| err.to_string())?;
                    }
                    Ok(())
                }
            }
        }
        _ => return Err("invalid arguments (see slatt --help)".into()),
    };
    result.map_err(|err| err.to_string())
}

fn main() {
    let args: Vec<_> = env::args().skip(1).collect();
    if let Err(error) = run(&args, io::stdout().lock()) {
        eprintln!("slatt: {error}");
        std::process::exit(1);
    }
}
