mod binary;
mod emit;
mod frontend;
mod ir;
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
  slatt run <rules.slatt> <input> --emit <lua51|luau> -o <output>
  slatt emit <lua51|luau> <input> <output>
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
        let ir = file(input, |source| {
            frontend::lower(&source_frontend, source, false)
        })?;
        if ir.is::<rhai::Blob>() {
            binary::import(&ir.cast::<rhai::Blob>())?
        } else {
            let ir = ir
                .try_cast::<rhai::ImmutableString>()
                .ok_or("source frontend must return PFIR text or bytecode bytes")?;
            parser::parse_ir(&ir, &script)?
        }
    } else {
        program(input, &script, false)?
    };
    for proto in &mut protos {
        for pass in &script.passes {
            pass.run(proto)?;
        }
    }
    Ok(protos)
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

fn write_bytecode(format: &str, protos: &[Proto], path: &str) -> Result<(), String> {
    let dialect = emit::Dialect::parse(format)?;
    let bytes = emit::emit(dialect, protos)?;
    write_new(path, &bytes)
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
        [verb, rule_path, input_path, flag, output_path] if verb == "recover" && flag == "-o" => {
            let (_, source_frontend) = rule_file(rule_path, None)?;
            let source_frontend = source_frontend.ok_or("recover requires a source frontend")?;
            let recovered = file(input_path, |source| {
                frontend::lower(&source_frontend, source, true)
            })?
            .try_cast::<rhai::ImmutableString>()
            .ok_or("frontend must return Lua source")?;
            if recovered.trim().is_empty() {
                return Err("frontend returned empty Lua source".into());
            }
            full_moon::parse(&recovered).map_err(|errors| errors[0].to_string())?;
            return write_new(output_path, recovered.as_bytes());
        }
        [verb, rule_path, input_path, mode, format, flag, path]
            if verb == "run" && mode == "--emit" && flag == "-o" =>
        {
            return write_bytecode(format, &run_passes(rule_path, input_path)?, path);
        }
        [verb, format, input_path, path] if verb == "emit" => {
            let protos = program(input_path, &Script::default(), false)?;
            return write_bytecode(format, &protos, path);
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
