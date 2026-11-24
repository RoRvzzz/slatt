mod binary;
mod ir;
mod parser;
mod rules;

use ir::{Facts, Proto};
use rules::Script;
use std::env;
use std::fs;
use std::io::{self, Read, Write};
use std::path::Path;

#[cfg(test)]
mod tests;

const HELP: &str = "Slatt — standalone bytecode pass language

Usage:
  slatt check <rules.slatt | ->
  slatt import <bytecode>
  slatt run <rules.slatt> <input>
  slatt facts <input> <proto> <block> <instruction>

run and facts accept .pfir, Lua 5.1 chunks, or Luau bytecode versions 6–14.
import and run print normalized IR to stdout. facts reports known constants
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
            let script = file(rule_path, parser::parse_script)?;
            writeln!(
                output,
                "OK: {} pass(es), {} opcode map(s)",
                script.passes.len(),
                script.maps.len()
            )
        }
        [verb, input_path] if verb == "import" => {
            let protos = program(input_path, &Script::default(), true)?;
            print_program(output, &protos)
        }
        [verb, rule_path, input_path] if verb == "run" => {
            let script = file(rule_path, parser::parse_script)?;
            let mut protos = program(input_path, &script, false)?;
            for proto in &mut protos {
                for pass in &script.passes {
                    pass.run(proto)?;
                }
            }
            print_program(output, &protos)
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
