use crate::ir::{Const, Op, Reg};
use crate::parser::{parse_ir, parse_script};

const RULES: &str = include_str!("../examples/demo.slatt");
const DEMO: &str = include_str!("../examples/demo.pfir");
const SIMPLIFY: &str = include_str!("../examples/simplify.slatt");

#[test]
fn project_frontend_maps_and_passes_run_as_one_chain() {
    let folder = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/project");
    let rules = folder.join("main.slatt");
    let input = folder.join("input.lua");
    let (script, frontend) = crate::rule_file(rules.to_str().unwrap(), None).unwrap();
    let order = script
        .passes
        .iter()
        .map(|pass| pass.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(order, ["constant_fold", "cleanup"]);
    assert!(script.maps.contains_key("mix_vm"));
    let result = crate::run_passes(rules.to_str().unwrap(), input.to_str().unwrap()).unwrap();
    let instructions = &result[0].blocks[0].instructions;
    assert_eq!(instructions.len(), 2);
    assert_eq!(instructions[0].to_string(), "LOADK(r2, 108)");
    assert_eq!(instructions[1].to_string(), "RETURN(r2)");
    let recovered = crate::frontend::lower(
        &frontend.unwrap(),
        &std::fs::read_to_string(input).unwrap(),
        true,
    )
    .unwrap()
    .cast::<rhai::ImmutableString>();
    assert_eq!(recovered, "return 108\n");
}

#[test]
fn project_includes_resolve_once_and_report_errors() {
    use std::{fs, time::SystemTime};

    let directory = std::env::temp_dir().join(format!(
        "slatt-project-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir(&directory).unwrap();
    struct TestDirectory(std::path::PathBuf);
    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    let directory = TestDirectory(directory);
    let path = |name| directory.0.join(name);
    fs::write(
        path("shared.slatt"),
        "frontend\nlet base_value = 40;\nfn answer() { 2 }\n%%\npass shared { dce; }",
    )
    .unwrap();
    fs::write(
        path("a.slatt"),
        "include \"shared.slatt\";\npass a { dce; }",
    )
    .unwrap();
    fs::write(
        path("b.slatt"),
        "include \"./shared.slatt\";\npass b { dce; }",
    )
    .unwrap();

    let root = path("unsaved.slatt");
    let load = |source| crate::project::load(source, &root, "-");
    let text = "// comment\ninclude \"a.slatt\";\ninclude \"b.slatt\";\nfrontend\n(base_value + answer()).to_string()\n%%";
    let (script, ast) = load(text).unwrap();
    assert_eq!(
        script
            .passes
            .iter()
            .map(|pass| pass.name.as_str())
            .collect::<Vec<_>>(),
        ["shared", "a", "b"]
    );
    let output = crate::frontend::lower(&ast.unwrap(), "", true)
        .unwrap()
        .cast::<rhai::ImmutableString>();
    assert_eq!(output, "42");

    let missing = load("\ninclude \"missing.slatt\";").err().unwrap();
    assert!(missing.starts_with("-:2:1: include "));
    let malformed = load("\ninclude \"a.slatt\"").err().unwrap();
    assert!(malformed.starts_with("-:2:"));
    let duplicate = load("include \"a.slatt\";\npass shared { dce; }")
        .err()
        .unwrap();
    assert!(duplicate.contains("duplicate project pass shared"));

    fs::write(
        path("b.slatt"),
        "include \"a.slatt\";\nfrontend\nfn answer() { 11 }\n%%",
    )
    .unwrap();
    let duplicate = load("include \"b.slatt\";").err().unwrap();
    assert!(duplicate.contains("duplicate project frontend function answer/0"));
    fs::write(path("a.slatt"), "include \"b.slatt\";").unwrap();
    let cycle = load("include \"a.slatt\";").err().unwrap();
    assert!(cycle.contains("include cycle"));
    assert!(cycle.contains("a.slatt") && cycle.contains("b.slatt"));

    fs::write(path("bad.slatt"), "frontend\nlet value = ;\n%%\n").unwrap();
    let syntax = load("include \"bad.slatt\";").err().unwrap();
    assert!(syntax.contains("bad.slatt:2:"), "{syntax}");
}

#[test]
fn source_frontend_receives_parsed_lua_tokens() {
    let (_, script) = crate::frontend::parse(
        "frontend\nif source_output { return lua_tokens(source); }\n42\n%%\n",
    )
    .unwrap();
    let script = script.unwrap();
    let converted = crate::frontend::lower(&script, "return (42) -- comment", true).unwrap();
    assert_eq!(converted.cast::<rhai::ImmutableString>(), "return ( 42 )");
    assert!(crate::frontend::lower(&script, "return (", true).is_err());
    assert_eq!(
        crate::frontend::lower(&script, "", false)
            .unwrap()
            .cast::<i64>(),
        42,
    );
}

#[test]
fn moonveil_dispatch_keeps_both_payload_arms_and_rejects_cycles() {
    let (_, script) = crate::frontend::parse(include_str!("../examples/moonveil.slatt")).unwrap();
    let script = script.unwrap();
    let source = r#"-- MoonVeil Obfuscator v1.2.2
local scratch,map,lookup,handlers,state
scratch={} map,lookup={[10]=20,[11]=30},function(k)return map[k+1]end
handlers={[20]=function() if flag then print("left") else print("right") end
    state=lookup(10);return true end}
state=lookup(9)
repeat while true do scratch[1]=handlers[state]
    if scratch[1]~=nil then if scratch[1]() then break end end
end until state==30"#;
    let output = crate::frontend::lower(&script, source, true)
        .unwrap()
        .cast::<rhai::ImmutableString>();
    assert!(output.contains(r#"if flag then print ( "left" ) else print ( "right" ) end"#));
    assert!(!output.contains("lookup"));
    let cycle = source.replace("state=lookup(10)", "state=lookup(9)");
    assert!(
        crate::frontend::lower(&script, &cycle, true)
            .unwrap_err()
            .contains("cyclic")
    );
}

#[test]
fn maps_folds_and_rewrites_a_raw_program() {
    let script = parse_script(RULES).unwrap();
    let mut program = parse_ir(DEMO, &script).unwrap();
    let proto = &mut program[0];
    assert_eq!(proto.blocks[0].instructions[0].to_string(), "LOADK(r0, 91)");
    assert_eq!(
        proto.facts().unwrap()[0][4].as_ref().unwrap()[&Reg(2)],
        Const::Number(91.0)
    );
    script.passes[0].run(proto).unwrap();
    assert_eq!(
        proto.blocks[0].instructions[2].to_string(),
        "LOADK(r2, 108)"
    );
    assert_eq!(proto.blocks[0].instructions[4].to_string(), "JUMP(@yes)");
    assert!(proto.facts().unwrap()[2][0].is_none());
    let result = proto.to_string();
    assert_eq!(parse_ir(&result, &script).unwrap()[0].to_string(), result);
}

#[test]
fn joins_only_keep_constants_agreed_on_by_reachable_predecessors() {
    let script = parse_script(RULES).unwrap();
    let source = r#"proto p {
        @entry: BR_TRUE(r0, @yes, @no);
        @yes: LOADK(r1, 4); LOADK(r2, nil); JUMP(@join);
        @no: LOADK(r1, 5); LOADK(r2, nil); JUMP(@join);
        @join: BR_EQ(r1, r2, @equal, @different);
        @equal: RETURN();
        @different: RETURN(r2);
    }"#;
    let mut protos = parse_ir(source, &script).unwrap();
    let proto = &mut protos[0];
    let facts = proto.facts().unwrap();
    let join = facts[3][0].as_ref().unwrap();
    assert!(!join.contains_key(&Reg(1)));
    assert_eq!(join[&Reg(2)], Const::Nil);
    script.passes[0].run(proto).unwrap();
    assert_eq!(proto.blocks[3].instructions[0].op, Op::BranchEq);
}

#[test]
fn loop_can_make_an_initially_impossible_edge_reachable() {
    let script = parse_script(RULES).unwrap();
    let source = r#"proto p {
        @entry: LOADK(r0, 0); LOADK(r1, 0); JUMP(@head);
        @head: BR_EQ(r0, r1, @body, @done);
        @body: LOADK(r0, 1); JUMP(@head);
        @done: RETURN(r0);
    }"#;
    let mut protos = parse_ir(source, &script).unwrap();
    let proto = &mut protos[0];
    let facts = proto.facts().unwrap();
    assert!(!facts[1][0].as_ref().unwrap().contains_key(&Reg(0)));
    assert!(facts[3][0].is_some());
    script.passes[0].run(proto).unwrap();
    assert_eq!(proto.blocks[1].instructions[0].op, Op::BranchEq);
}

#[test]
fn a_pattern_can_bind_and_reuse_registers_across_instructions() {
    let script = parse_script(
        r#"pass tidy {
        match [LOADK($r, $c), MOVE($r, $r)] where known($c) and const($c) == 4
            => [LOADK($r, $c)];
    }"#,
    )
    .unwrap();
    let mut protos = parse_ir(
        r#"proto p {
        @entry: LOADK(r0, 4); MOVE(r0, r0); LOADK(r1, 4); MOVE(r2, r1); RETURN(r2);
    }"#,
        &script,
    )
    .unwrap();
    script.passes[0].run(&mut protos[0]).unwrap();
    let ops: Vec<_> = protos[0].blocks[0]
        .instructions
        .iter()
        .map(|inst| inst.op)
        .collect();
    assert_eq!(ops, [Op::LoadK, Op::LoadK, Op::Move, Op::Return]);
}

#[test]
fn signed_zero_facts_stay_distinct_and_nonfinite_results_are_unknown() {
    let script = parse_script("pass fold_all { fold constants; }").unwrap();
    let source = r#"proto p {
        @entry: LOADK(r2, 1); BR_TRUE(r0, @yes, @no);
        @yes: LOADK(r1, -0); JUMP(@join);
        @no: LOADK(r1, 0); JUMP(@join);
        @join: DIV(r3, r2, r1); DIV(r4, r2, r1); RETURN(r3);
    }"#;
    let mut protos = parse_ir(source, &script).unwrap();
    let proto = &mut protos[0];
    let facts = proto.facts().unwrap();
    assert!(!facts[3][1].as_ref().unwrap().contains_key(&Reg(1)));
    assert!(!facts[3][1].as_ref().unwrap().contains_key(&Reg(3)));
    script.passes[0].run(proto).unwrap();
    assert_eq!(proto.blocks[3].instructions[0].op, Op::Div);
}

#[test]
fn rejects_invalid_bindings_and_opcode_maps() {
    for (source, expected) in [
        (
            "pass p { match [MOVE($a, $b)] => [MOVE($a, $c)]; }",
            "unknown binding $c",
        ),
        ("pass p { match [LOADK($a, $a)] => []; }", "has type Reg"),
        (
            "pass p { match [] => [RETURN()]; }",
            "at least one instruction",
        ),
        (
            "map p { 1 => MOVE($0, $1); 1 => RETURN(); }",
            "duplicate raw opcode",
        ),
        ("map p { 1 => LOADK($0, $0); }", "conflicting types"),
    ] {
        assert!(
            parse_script(source).err().unwrap().contains(expected),
            "{source}"
        );
    }
    let script = parse_script("map p { 1 => LOADK($0, $1); } ").unwrap();
    for (source, expected) in [
        (
            "raw proto a using p { @entry: 2(); }",
            "unmapped raw opcode 2",
        ),
        (
            "raw proto a using p { @entry: 1(r0); }",
            "missing raw operand $1",
        ),
        ("raw proto a using p { @entry: 1(7, r0); }", "expects Reg"),
        (
            "proto a { @entry: JUMP(@missing); }",
            "missing block @missing",
        ),
        (
            "proto a { @entry: RETURN(); RETURN(); }",
            "exactly one terminator",
        ),
    ] {
        assert!(
            parse_ir(source, &script).err().unwrap().contains(expected),
            "{source}"
        );
    }
}

#[test]
fn invalid_replacement_fails_verification() {
    let script = parse_script("pass p { match [RETURN()] => [JUMP(@missing)]; }").unwrap();
    let mut protos = parse_ir("proto a { @entry: RETURN(); }", &script).unwrap();
    let error = script.passes[0].run(&mut protos[0]).unwrap_err();
    assert!(error.contains("pass p, prototype a"));
    assert!(error.contains("missing block @missing"));
}

#[test]
fn string_and_number_literals_round_trip() {
    let source = "\u{feff}proto p { @entry: LOADK(r0, \"é\\n\\u{1}\"); LOADK(r1, -2.5e+2); RETURN(r0, r1); }";
    let script = parse_script("").unwrap();
    let first = parse_ir(source, &script).unwrap().remove(0);
    let written = first.to_string();
    let second = parse_ir(&written, &script).unwrap().remove(0);
    assert_eq!(first.blocks[0].instructions, second.blocks[0].instructions);
}

#[test]
fn guarded_arms_choose_first_match_and_skip_unknown_operands() {
    let script = parse_script(SIMPLIFY).unwrap();
    for (initializers, expected_block) in [
        ("LOADK(r0, 7); LOADK(r1, 7);", Some("yes")),
        ("LOADK(r0, 7); LOADK(r1, 8);", Some("no")),
        ("LOADK(r0, nil); LOADK(r1, nil);", Some("yes")),
        ("LOADK(r0, 7);", None),
    ] {
        let source = format!(
            "proto p {{ @entry: {initializers} BR_EQ(r0, r1, @yes, @no); @yes: RETURN(); @no: RETURN(); }}"
        );
        let mut program = parse_ir(&source, &script).unwrap();
        let proto = &mut program[0];
        script.passes[0].run(proto).unwrap();
        if let Some(block) = expected_block {
            assert_eq!(proto.blocks.len(), 2);
            assert_eq!(proto.blocks[1].name, block);
            assert_eq!(proto.blocks[0].instructions.len(), 1);
            assert_eq!(
                proto.blocks[0].instructions[0].to_string(),
                format!("JUMP(@{block})")
            );
        } else {
            assert_eq!(proto.blocks.len(), 3);
            assert_eq!(
                proto.blocks[0].instructions.last().unwrap().op,
                Op::BranchEq
            );
        }
    }
}

#[test]
fn compares_both_ordered_branch_outcomes() {
    let script = parse_script(SIMPLIFY).unwrap();
    for (comparison, right, expected) in [
        ("BR_LT", 8, "yes"),
        ("BR_LT", 7, "no"),
        ("BR_LE", 7, "yes"),
        ("BR_LE", 6, "no"),
    ] {
        let source = format!(
            "proto p {{ @entry: LOADK(r0, 7); LOADK(r1, {right}); {comparison}(r0, r1, @yes, @no); @yes: RETURN(); @no: RETURN(); }}"
        );
        let mut program = parse_ir(&source, &script).unwrap();
        script.passes[0].run(&mut program[0]).unwrap();
        assert_eq!(program[0].blocks[1].name, expected);
    }
}

#[test]
fn dce_keeps_effects_and_values_used_across_blocks() {
    let script = parse_script("pass trim { prune unreachable; dce; }").unwrap();
    let source = r#"proto p {
        @entry: LOADK(r2, 5); LOADK(r3, 6); JUMP(@next);
        @unreachable: NATIVE("lua51.CALL", 28); RETURN();
        @next: ADD(r4, r0, r2); RETURN(r2);
    }"#;
    let mut program = parse_ir(source, &script).unwrap();
    script.passes[0].run(&mut program[0]).unwrap();
    let text = program[0].to_string();
    assert!(text.contains("LOADK(r2, 5)"));
    assert!(text.contains("ADD(r4, r0, r2)"));
    assert!(!text.contains("LOADK(r3, 6)"));
    assert!(!text.contains("@unreachable"));
}

#[test]
fn stock_compiler_branches_can_be_simplified() {
    let script = parse_script(SIMPLIFY).unwrap();
    for chunk in [
        include_bytes!("../tests/fixtures/branch-lua51.luac").as_slice(),
        include_bytes!("../tests/fixtures/branch-luau-0.748.luauc").as_slice(),
    ] {
        let mut program = crate::binary::import(chunk).unwrap();
        assert!(program[0].to_string().contains("BR_EQ("));
        script.passes[0].run(&mut program[0]).unwrap();
        let text = program[0].to_string();
        assert!(text.contains("\"equal\""));
        assert!(!text.contains("\"different\""));
        assert!(!text.contains("BR_EQ("));
        assert!(!text.contains(", 19)"));
    }
}

#[test]
fn compiler_chunks_preserve_native_operations_and_binary_strings() {
    let script = parse_script(SIMPLIFY).unwrap();
    for chunk in [
        include_bytes!("../tests/fixtures/native-lua51.luac").as_slice(),
        include_bytes!("../tests/fixtures/native-luau-workspace.luauc").as_slice(),
        include_bytes!("../tests/fixtures/native-luau-0.748.luauc").as_slice(),
    ] {
        let mut program = crate::binary::import(chunk).unwrap();
        assert_eq!(program.len(), 2);
        for proto in &mut program {
            script.passes[0].run(proto).unwrap();
        }
        let text = program[0].to_string();
        assert!(text.contains(".CALL\""));
        assert!(text.contains("NATIVE_BRANCH("));
        assert!(text.contains("\\xffok"));
        assert!(
            text.contains(", 17)"),
            "escaped capture must retain the final write"
        );
        let reread = parse_ir(&text, &script).unwrap();
        assert_eq!(reread[0].to_string(), text);
    }
}

#[test]
fn luau_v14_fastcalls_keep_both_continuations() {
    let chunk = include_bytes!("../tests/fixtures/native-luau-0.748-O2.luauc");
    let protos = crate::binary::import(chunk).unwrap();
    let ir = protos[0].to_string();
    assert!(ir.contains("NATIVE_BRANCH(\"luau.FASTCALL1\""));
    assert!(ir.contains("NATIVE_BRANCH(\"luau.FASTPCALL\""));
    assert!(ir.contains("@pc27, @pc24"));
    assert!(protos[0].verify().is_ok());
}

#[test]
fn rejects_every_truncated_prefix_of_compiled_chunks() {
    for chunk in [
        include_bytes!("../tests/fixtures/branch-lua51.luac").as_slice(),
        include_bytes!("../tests/fixtures/branch-luau-0.748.luauc").as_slice(),
    ] {
        for end in 0..chunk.len() {
            assert!(
                crate::binary::import(&chunk[..end]).is_err(),
                "accepted truncated prefix {end}"
            );
        }
    }
}

#[test]
fn custom_vm_rules_remove_the_decoy_and_emit_both_native_formats() {
    use crate::emit::{Dialect, emit};
    let file = include_str!("../examples/deobfuscator/deobfuscate.slatt");
    let fixture = include_str!("../examples/deobfuscator/obfuscated.pfir");
    let script = parse_script(file).unwrap();
    let mut program = parse_ir(fixture, &script).unwrap();
    for pass in &script.passes {
        pass.run(&mut program[0]).unwrap();
    }
    let rewritten = program[0].to_string();
    assert!(!rewritten.contains("DECOY"));
    assert!(!rewritten.contains("XOR32"));
    assert!(rewritten.contains("BR_LT(r9, r6, @lower, @higher)"));
    let reread = parse_ir(&rewritten, &script).unwrap();
    for dialect in [Dialect::Lua51, Dialect::Luau] {
        let bytes = emit(dialect, &reread).unwrap();
        let lifted = crate::binary::import(&bytes).unwrap();
        assert!(lifted[0].variadic);
        assert_eq!(lifted[0].params, 0);
        assert!(lifted[0].to_string().contains("VARARG(r9)"));
        assert!(lifted[0].to_string().contains("\"high\""));
    }
}

#[test]
fn emitter_rejects_opaque_records_and_target_frame_overflow() {
    use crate::emit::{Dialect, emit};
    let script = parse_script("").unwrap();
    for (ir, expected) in [
        (
            r#"proto p { @entry: NATIVE("lua51.CALL", 28); RETURN(); }"#,
            "opaque instruction",
        ),
        (
            "proto p { @entry: LOADK(r250, 9); RETURN(r250); }",
            "register frame needs 251",
        ),
        (
            "proto p { @entry: XOR32(r0, r1, r2); RETURN(r0); }",
            "XOR32 needs a known result",
        ),
    ] {
        let program = parse_ir(ir, &script).unwrap();
        assert!(
            emit(Dialect::Lua51, &program)
                .unwrap_err()
                .contains(expected)
        );
    }
}

#[test]
fn emission_preserves_parameter_count_and_binary_constants() {
    use crate::emit::{Dialect, emit};
    let script = parse_script("").unwrap();
    let source = r#"proto p params 2 vararg {
      @entry: VARARG(r5, r2); LOADK(r3, "\xff\x00"); LOADK(r4, bits("7ff8000000012345")); RETURN(r1, r3, r4, r2, r5);
    }"#;
    let program = parse_ir(source, &script).unwrap();
    for dialect in [Dialect::Lua51, Dialect::Luau] {
        let bytes = emit(dialect, &program).unwrap();
        let lifted = crate::binary::import(&bytes).unwrap();
        assert_eq!(lifted[0].params, 2);
        assert!(lifted[0].variadic);
        let text = lifted[0].to_string();
        assert!(text.contains("\"\\xff\\x00\""));
        assert!(text.contains("bits(\"7ff8000000012345\")"));
    }
}

#[test]
fn threading_shortens_jump_chains_and_preserves_cycles() {
    let script = parse_script("pass clean { thread jumps; prune unreachable; }").unwrap();
    let source = r#"proto p {
        @entry: BR_TRUE(r0, @next, @loop_a);
        @next: JUMP(@again);
        @again: JUMP(@done);
        @done: RETURN();
        @loop_a: JUMP(@loop_b);
        @loop_b: JUMP(@loop_a);
    }"#;
    let mut program = parse_ir(source, &script).unwrap();
    script.passes[0].run(&mut program[0]).unwrap();
    let ir = program[0].to_string();
    assert!(ir.contains("BR_TRUE(r0, @done, @loop_a)"));
    assert!(!ir.contains("@next"));
    assert!(!ir.contains("@again"));
    assert!(ir.contains("@loop_a:\n    JUMP(@loop_b)"));
    assert!(ir.contains("@loop_b:\n    JUMP(@loop_a)"));
}

#[test]
fn lua51_round_trip_retains_native_loops_and_child_closures() {
    use crate::emit::{Dialect, emit};
    let original = include_bytes!("../tests/fixtures/native-lua51.luac");
    let script = parse_script(include_str!("../examples/moonsec/recover.slatt")).unwrap();
    let mut program = crate::binary::import(original).unwrap();
    for proto in &mut program {
        script.passes[0].run(proto).unwrap();
    }
    let emitted = emit(Dialect::Lua51, &program).unwrap();
    let replay = crate::binary::import(&emitted).unwrap();
    assert_eq!(replay.len(), 2);
    for opcode in [
        "lua51.CLOSURE",
        "lua51.FORPREP",
        "lua51.FORLOOP",
        "lua51.CALL",
    ] {
        assert!(replay[0].to_string().contains(opcode));
    }
    assert!(replay[1].to_string().contains("lua51.SETUPVAL"));

    let native = program[0]
        .blocks
        .iter_mut()
        .flat_map(|block| &mut block.instructions)
        .find(|inst| inst.op == Op::Native && inst.args.len() > 1)
        .unwrap();
    native.args[1] = crate::ir::Operand::Const(Const::Number(5.0));
    assert!(
        emit(Dialect::Lua51, &program)
            .unwrap_err()
            .contains("must come unchanged")
    );
}

#[test]
fn source_frontend_accepts_larger_token_lists_with_an_input_bound() {
    let (_, frontend) =
        crate::frontend::parse("frontend\nlua_token_list(source).len\n%%\n").unwrap();
    let frontend = frontend.unwrap();
    let source = "do end\n".repeat(33_000);
    let tokens = crate::frontend::lower(&frontend, &source, false).unwrap();
    assert_eq!(tokens.as_int().unwrap(), 66_000);

    let oversized = " ".repeat((4 << 20) + 1);
    assert_eq!(
        crate::frontend::lower(&frontend, &oversized, false).unwrap_err(),
        "source frontend input exceeds 4 MiB"
    );
}

#[test]
fn source_frontend_token_buffer_preserves_luau_and_generic_access() {
    let (_, frontend) = crate::frontend::parse(
        "frontend\nlet tokens = lua_token_buffer(source);\n\
         let compound = tokens.index_of(\"+=\", 0);\n\
         if tokens.len != 7 || tokens[compound] != \"+=\" || !tokens.contains(\"x\") { throw \"tokens\"; }\n\
         if tokens.extract(compound, 2) != [\"+=\", \"2\"] { throw \"extract\"; }\n\
         tokens.join_lua_tokens(compound, tokens.len)\n%%\n",
    )
    .unwrap();
    let result = crate::frontend::lower(&frontend.unwrap(), "local x = 1 x += 2", false).unwrap();
    assert_eq!(result.into_string().unwrap(), "+= 2 ");
}

#[test]
fn source_frontend_can_parse_bounded_regions_and_edit_original_source() {
    let rule = r#"frontend
let data = blob();
for byte in [1, 2, 129, 1, 90] { data.push(byte); }
let alphabet = blob();
for symbol in 33..118 { alphabet.push(symbol); }
let spelling = bytes("!!!!! !!!!\"");
let first = spelling.extract(0,5);
let last = spelling.extract(6,5);
let be = byte_cursor(last.decode_base85(alphabet,true));
let le = byte_cursor(last.decode_base85(alphabet,false));
if be.read_uint(4,true) != 1 || le.read_uint(4,false) != 1 { throw "Base85 byte order"; }
if first.decode_base85(alphabet,true).len != 4 { throw "Base85 word size"; }
let cursor = byte_cursor(data);
if cursor.read_uint(2, true) != 258 { throw "endian"; }
let child = cursor.substream(2);
if child.offset != 2 || child.read_varuint() != 129 || child.remaining != 0 { throw "child"; }
try { child.read_uint(1, false); throw "read crossed its region"; }
catch (error) { if !error.contains("0x4") { throw error; } }
if cursor.take(1).as_string() != "Z" || cursor.remaining != 0 { throw "parent"; }
let tokens = lua_token_buffer(source);
let span = tokens.token_span(tokens.index_of("=", 0));
splice_bytes(source, span[0], span[1], "=")
%%
"#;
    let (_, frontend) = crate::frontend::parse(rule).unwrap();
    let source = "-- naïve\nlocal x = 'é'";
    let result = crate::frontend::lower(&frontend.unwrap(), source, false).unwrap();
    assert_eq!(result.into_string().unwrap(), source);
}

#[test]
fn luau_trace_preserves_nil_slots_and_limits_host_access() {
    let script = r#"
        local before = typeof(buffer.create(1))
        x = host.answer()
        __slatt_probe("stage", x, nil)
        print("value", x)
        return before, nil, x, missing, require, getfenv(0).require, os.execute
    "#;
    let fixture = "return { host = { answer = function() return 42 end } }";
    let report = crate::luau_runtime::trace(script, fixture, "=probe").unwrap();
    assert_eq!(report["ok"], true, "{}", report["error"]);
    let returns = report["returns"].as_array().unwrap();
    assert_eq!(returns.len(), 7);
    assert_eq!(returns[0]["value"], "buffer");
    assert_eq!(returns[2]["value"], 42);
    for index in [1, 3, 4, 5, 6] {
        assert_eq!(returns[index]["type"], "nil");
    }
    let events = report["events"].as_array().unwrap();
    assert!(
        events
            .iter()
            .any(|e| e["key"] == "missing" && e["available"] == false)
    );
    assert!(
        events
            .iter()
            .any(|e| e["event"] == "global_insert" && e["key"] == "x")
    );
    assert!(
        events
            .iter()
            .any(|e| e["event"] == "probe" && e["args"][1]["type"] == "nil")
    );
    assert!(
        events
            .iter()
            .any(|e| e["event"] == "print" && e["args"][1]["value"] == 42)
    );
}

#[test]
fn embedded_capture_preserves_bytes_and_replay_reports_mismatches() {
    let script = r#"local buf=buffer.fromstring("\000\255")
__slatt_dump("before", buf)
buffer.writeu8(buf, 1, 13)
__slatt_dump("after", buffer.tostring(buf))
print("probe", nil)
return 7, nil, "\255""#;
    let run = crate::luau_runtime::capture(script, "", "=capture").unwrap();
    assert_eq!(run.report["ok"], true, "{}", run.report["error"]);
    assert_eq!(run.artifacts["before"], [0, 255]);
    assert_eq!(run.artifacts["after"], [0, 13]);
    assert_eq!(
        crate::luau_runtime::replay(script, script, "").unwrap()["matched"],
        true
    );
    assert_eq!(
        crate::luau_runtime::replay(script, &script.replace("7, nil", "8, nil"), "").unwrap()["matched"],
        false
    );
    let twice = "__slatt_dump('duplicate','a')\n__slatt_dump('duplicate','b')";
    let run = crate::luau_runtime::capture(twice, "", "=duplicate").unwrap();
    assert_eq!(run.report["ok"], false);
    assert!(
        run.report["error"]
            .as_str()
            .unwrap()
            .contains("duplicate Slatt capture")
    );
    assert_eq!(run.artifacts["duplicate"], b"a");

    let binary_label = r#"__slatt_probe("\255")"#;
    let different_label = r#"__slatt_probe("\254")"#;
    let result = crate::luau_runtime::replay(binary_label, different_label, "").unwrap();
    assert_eq!(result["matched"], false, "{result}");
    let opaque_label = "__slatt_probe({})";
    let result = crate::luau_runtime::replay(opaque_label, opaque_label, "").unwrap();
    assert_eq!(result["matched"], false, "{result}");
}

#[test]
fn replay_command_reports_matches_and_returns_an_error_for_mismatches() {
    let examples = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples");
    let branch = examples.join("branch.lua").to_str().unwrap().to_owned();
    let project = examples
        .join("project/input.lua")
        .to_str()
        .unwrap()
        .to_owned();
    for (recovered, matched) in [(branch.clone(), true), (project, false)] {
        let mut output = Vec::new();
        let result = crate::run(&["replay".into(), branch.clone(), recovered], &mut output);
        let report: serde_json::Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(report["matched"], matched, "{report}");
        assert_eq!(result.is_ok(), matched, "{result:?}");
    }
}

#[test]
fn packed_codecs_decode_partial_words_and_reject_malformed_streams() {
    let rule = r#"
frontend
let encoded = bytes("87cURDZ");
if encoded.decode_ascii85().as_string() != "Hello" { throw "Ascii85 tail"; }
let column = bytes("nnbaaa");
if column.inverse_bwt(3).as_string() != "banana" { throw "inverse BWT"; }
let lengths = blob(257, 0);
lengths[1] = 1; lengths[256] = 1;
let table = lengths.canonical_huffman();
let data = blob(); data.push(25);
let bits = bit_cursor_lsb(data);
let result = bits.read_mtf_runs(table, 3, 256);
if result.len != 3 || result[0] != 1 || result[1] != 0 || result[2] != 1 ||
   bits.remaining != 3 { throw "move-to-front ranks"; }
let bits = bit_cursor_lsb(data);
let failed = false;
try { bits.read_mtf_runs(table, 2, 256); } catch { failed = true; }
if !failed || bits.remaining != 8 { throw "oversized run advanced the cursor"; }
lengths[0] = 1;
let failed = false;
try { lengths.canonical_huffman(); } catch { failed = true; }
if !failed { throw "oversubscribed Huffman table accepted"; }
let bytes = blob(); for b in [255,255,255,255,31] { bytes.push(b); }
let cursor = byte_cursor(bytes);
let failed = false;
try { cursor.vu32(); } catch { failed = true; }
if !failed || cursor.remaining != 5 { throw "overflowing varuint advanced the cursor"; }
let bits = bit_cursor_lsb(blob(1, 24));
if bits.read_rice(2, 13) != 13 || bits.remaining != 2 { throw "Rice value"; }
let ranks = blob(5, 1);
let expected = blob(); for byte in [1,1,0,0,1] { expected.push(byte); }
if ranks.decode_mtf(8) != expected { throw "contextual move-to-front"; }
let explicit = prefix_huffman([[11, 1, 1], [22, 2, 0], [33, 2, 1]]);
let bits = bit_cursor_lsb(blob(1, 3));
if bits.read_huffman(explicit) != 11 || bits.read_huffman(explicit) != 11 ||
   bits.read_huffman(explicit) != 22 { throw "explicit prefix codebook"; }
let failed = false;
try { prefix_huffman([[10, 1, 0], [20, 2, 1]]); } catch { failed = true; }
if !failed { throw "overlapping prefix codes accepted"; }
"ok"
%%
"#;
    let (_, frontend) = crate::frontend::parse(rule).unwrap();
    let result = crate::frontend::lower(&frontend.unwrap(), "", false).unwrap();
    assert_eq!(result.cast::<rhai::ImmutableString>(), "ok");
}

#[test]
fn linear_frontend_builds_blocks_and_keeps_implicit_fallthrough() {
    let rule = r#"
frontend
let code = [
    pfir_asm("GETGLOBAL", [0, "choose"]),
    pfir_asm("LOADK", [1, "A"]),
    pfir_asm("BR_TRUE", [0, 3, 4]),
    pfir_asm("LOADK", [1, "B"]),
    pfir_asm("RETURN", [1])
];
let bad = code;
bad[2] = pfir_inst("JUMP", [pfir_pc(5)]);
let rejected = false;
try { pfir_linear("test", 0, false, bad); } catch { rejected = true; }
if !rejected { throw "accepted out-of-range branch"; }
let result = pfir_linear("test", 0, false, code);
if result.cfg()[1].successors[0] != "pc4" { throw "missing fallthrough"; }
result.emit_lua()
%%
"#;
    let (_, frontend) = crate::frontend::parse(rule).unwrap();
    let recovered = crate::frontend::lower(&frontend.unwrap(), "", true)
        .unwrap()
        .cast::<rhai::ImmutableString>();
    for fixture in ["return {choose = true}", "return {choose = false}"] {
        let report =
            crate::luau_runtime::replay("return choose and 'B' or 'A'", &recovered, fixture)
                .unwrap();
        assert_eq!(report["matched"], true, "{report}");
    }
}

#[test]
fn token_span_matches_first_complete_handler_without_reparsing() {
    let rule = r#"
frontend
let tokens = lua_token_buffer(source);
let first = tokens.find_tokens(["r", "["], 0);
let last = tokens.find_tokens([";"], first);
let bank = [lua_token_buffer("r[k]=nil"), lua_token_buffer("r[__match_slot]=r[__match_slot]"),
            lua_token_buffer("r[__match_dest]=r[__match_src]")];
let result = tokens.match_span(tokens.token_span(first)[0], tokens.token_span(last)[1], bank);
if result == () { "miss" } else { result[0].to_string() + ":" + (result[1].slot ?? result[1].dest) }
%%
"#;
    let (_, frontend) = crate::frontend::parse(rule).unwrap();
    let frontend = frontend.unwrap();
    for (source, expected) in [
        ("-- π\nif x then r[key]=r[key]; end", "1:key"),
        ("if x then r[key]=r[other]; end", "2:key"),
        ("if x then r[key]=7; end", "miss"),
    ] {
        let result = crate::frontend::lower(&frontend, source, false).unwrap();
        assert_eq!(result.cast::<rhai::ImmutableString>(), expected);
    }
    let invalid = rule.replace("token_span(last)[1]", "token_span(last)[1]+1");
    let (_, frontend) = crate::frontend::parse(&invalid).unwrap();
    assert!(
        crate::frontend::lower(&frontend.unwrap(), "r[key]=r[key];", false)
            .unwrap_err()
            .contains("complete tokens")
    );
}

#[test]
fn whole_handler_patterns_bind_repeated_fields_and_reject_extra_statements() {
    let rule = r#"
frontend
let pattern = lua_token_buffer("local __match_local=h[__match_field]; r[__match_local]=r[__match_local+1]");
let handler = lua_token_buffer(source);
let bindings = handler.match_pattern(pattern);
if bindings == () { "no match" } else { bindings.local + ":" + bindings.field }
%%
"#;
    let (_, frontend) = crate::frontend::parse(rule).unwrap();
    let frontend = frontend.unwrap();
    for (source, expected) in [
        ("local x=h[4753]; r[x]=r[x+1]; -- suffix", "x:4753"),
        ("local x=h[4753]; r[x]=r[y+1];", "no match"),
        ("local x=h[4753]; r[x]=r[x+1]; r[0]=0;", "no match"),
    ] {
        let result = crate::frontend::lower(&frontend, source, false).unwrap();
        assert_eq!(result.cast::<rhai::ImmutableString>(), expected);
    }
}

#[test]
fn linear_bundles_preserve_physical_addresses_and_table_side_effects() {
    let rule = r#"
frontend
let code = [
    [pfir_asm("LOADK", [1,"key"]), pfir_asm("LOADK", [2,7])],
    pfir_asm("BR_TRUE", [0,2,4]),
    [pfir_asm("NEWTABLE", [0]), pfir_asm("SETTABLE", [0,1,2])],
    [pfir_asm("GETTABLE", [2,0,1]), pfir_asm("POW",[2,2,2])],
    pfir_asm("RETURN", [2])
];
let prototype = pfir_linear("test", 1, false, code);
let cases = prototype.cfg();
if cases[0].successors != ["pc2","pc4"] { throw "physical branch targets"; }
let rejected = false;
try {
    pfir_linear("bad",0,false,[[pfir_asm("RETURN",[]),pfir_asm("MOVE",[0,1])]]);
} catch { rejected = true; }
if !rejected { throw "accepted early bundle terminator"; }
prototype.pfir_text()
%%
"#;
    let (_, frontend) = crate::frontend::parse(rule).unwrap();
    let text = crate::frontend::lower(&frontend.unwrap(), "", false)
        .unwrap()
        .cast::<rhai::ImmutableString>();
    let clean = parse_script("pass cleanup { fold constants; dce; }").unwrap();
    let mut program = parse_ir(&text, &clean).unwrap();
    crate::apply_passes(&clean, &mut program).unwrap();
    let source = crate::lua_emit::emit_program(&program).unwrap();
    let reference = "local o={} o.key=7 return o.key^o.key";
    let candidate = format!("return (function(...)\n{source}\nend)(true)");
    let report = crate::luau_runtime::replay(reference, &candidate, "").unwrap();
    assert_eq!(report["matched"], true, "{report}");
}

#[test]
fn inherited_captures_share_closed_cells_across_register_reuse() {
    let script = parse_script("pass simplify { fold constants; dce; }").unwrap();
    let mut program = parse_ir(
        r#"
        proto root {
            @entry:
                LOADK(r0,5); LOADK(r1,20);
                CLOSURE(r2,"maker",r0); CALL(r2,2,r3,r4);
                CLOSURE(r5,"get",r0); CLOSURE(r6,"get",r1);
                CLOSEUPVALS(r1);
                LOADK(r1,200); LOADK(r0,7);
                CLOSEUPVALS(r0);
                LOADK(r0,100); CLOSURE(r7,"get",r0);
                CALL(r3,1,r8); CALL(r4,1,r9); CALL(r5,1,r10);
                CALL(r6,1,r11); CALL(r7,1,r12);
                RETURN(r8,r9,r10,r11,r12);
        }
        proto maker {
            @entry: CLOSURE(r0,"inc",0); CLOSURE(r1,"get",0); RETURN(r0,r1);
        }
        proto inc {
            @entry: GETUPVAL(r0,0); LOADK(r1,1); ADD(r0,r0,r1); SETUPVAL(r0,0); RETURN(r0);
        }
        proto get {
            @entry: GETUPVAL(r0,0); RETURN(r0);
        }
        "#,
        &script,
    )
    .unwrap();
    crate::apply_passes(&script, &mut program).unwrap();
    let source = crate::lua_emit::emit_program(&program).unwrap();
    let report = crate::luau_runtime::replay("return 8,8,8,20,100", &source, "").unwrap();
    assert_eq!(report["matched"], true, "{report}\n{source}");
    assert!(
        crate::lua_emit::emit_program(&program[1..])
            .unwrap_err()
            .contains("root prototype")
    );
}

#[test]
fn rewrite_validates_inherited_capture_descriptors_before_installing_them() {
    let script = parse_script(
        r#"pass rewrite { match [LOADK($reg,$slot)] => [CLOSURE($reg,"child",$slot)]; }"#,
    )
    .unwrap();
    let mut program = parse_ir(
        r#"proto root { @entry: LOADK(r0,255); RETURN(r0); }"#,
        &script,
    )
    .unwrap();
    let error = crate::apply_passes(&script, &mut program).unwrap_err();
    assert!(
        error.contains("captures must be registers or upvalue slots"),
        "{error}"
    );
    assert_eq!(program[0].blocks[0].instructions[0].op, Op::LoadK);
}

#[test]
fn jump_table_keeps_duplicate_destinations_and_coerces_the_index_once() {
    let script = parse_script("pass simplify { thread jumps; prune unreachable; dce; }").unwrap();
    let mut program = parse_ir(
        r#"
        proto number params 1 {
            @entry: JUMP_TABLE(r0,@left,@redirect,@right);
            @left: LOADK(r1,11); RETURN(r1);
            @redirect: JUMP(@right);
            @right: LOADK(r1,22); RETURN(r1);
        }
        "#,
        &script,
    )
    .unwrap();
    crate::apply_passes(&script, &mut program).unwrap();
    let source = crate::lua_emit::emit_program(&program).unwrap();
    let run = r#"
        local key=setmetatable({}, {__add=function(_,v) __slatt_probe('add',v); return 2 end})
        local success=pcall(jump,3)
        return jump(0),jump(1),jump(2),jump(key),success
    "#;
    let reference =
        format!("local function jump(k) local t={{11,22,22}} return assert(t[k+1]) end\n{run}");
    let candidate = format!("local function jump(...)\n{source}\nend\n{run}");
    let report = crate::luau_runtime::replay(&reference, &candidate, "").unwrap();
    assert_eq!(report["matched"], true, "{report}");
}

#[test]
fn large_dispatch_preserves_contiguous_targets_and_selector_errors() {
    let targets = (0..33)
        .map(|i| format!("@case{i}"))
        .collect::<Vec<_>>()
        .join(",");
    let mut rule = format!("proto jump params 1 {{ @entry: JUMP_TABLE(r0,{targets});");
    for i in 0..33 {
        rule.push_str(&format!("@case{i}: LOADK(r1,{i}); RETURN(r1);"));
    }
    rule.push('}');
    let program = parse_ir(&rule, &parse_script("").unwrap()).unwrap();
    let source = crate::lua_emit::emit_program(&program).unwrap();
    assert!(source.contains("for i = 1, 33 do"), "{source}");
    assert!(source.contains("_block < 19"), "{source}");
    let run = r#"
        for i=0,32 do assert(jump(i)==i) end
        for _,invalid in ipairs({-1,0.5,33,math.huge,0/0}) do
            assert(not pcall(jump,invalid))
        end
        local key=setmetatable({}, {__add=function(_,v) __slatt_probe("add",v); return 33 end})
        local wrong=setmetatable({}, {__add=function() return "1" end})
        local ok=pcall(jump,wrong)
        return jump(key),ok
    "#;
    let reference = format!(
        "local function jump(k) local t={{}} for i=1,33 do t[i]=i-1 end return assert(t[k+1]) end\n{run}"
    );
    let candidate = format!("local function jump(...)\n{source}\nend\n{run}");
    let report = crate::luau_runtime::replay(&reference, &candidate, "").unwrap();
    assert_eq!(report["matched"], true, "{report}");
}

#[test]
fn receiver_binding_list_writes_and_tail_calls_preserve_lua_effects() {
    let script = parse_script("pass simplify { dce; }").unwrap();
    let mut program = parse_ir(
        r#"
        proto apply params 4 {
            @entry:
                SETLIST(r0,101,r2,r3);
                SELF(r0,r1,r0,r1);
                TAILCALL(r0,r1,r2,r3);
        }
        "#,
        &script,
    )
    .unwrap();
    crate::apply_passes(&script, &mut program).unwrap();
    let source = crate::lua_emit::emit_program(&program).unwrap();
    let run = r#"
        local object=setmetatable({}, {
            __newindex=function(_,k,v) __slatt_probe('write',k,v) end,
            __index=function(t,k)
                __slatt_probe('read',k)
                return function(receiver,x,y)
                    assert(receiver==t)
                    return x,y,nil
                end
            end,
        })
        return apply(object,'method',5,nil)
    "#;
    let reference =
        format!("local function apply(o,k,x,y) o[101]=x o[102]=y return o[k](o,x,y) end\n{run}");
    let candidate = format!("local function apply(...)\n{source}\nend\n{run}");
    let report = crate::luau_runtime::replay(&reference, &candidate, "").unwrap();
    assert_eq!(report["matched"], true, "{report}");
}

#[test]
fn packed_lua_numbers_preserve_bits_and_apply_lua_integer_rounding() {
    let rule = r#"
frontend
let p = pfir_linear("numbers",0,false,[
    pfir_asm("LOADK",[0,lua_number_from_bits(-9223372036854775807-1,"f64")]),
    pfir_asm("LOADK",[1,lua_number_from_bits(-1,"u64")]),
    pfir_asm("LOADK",[2,lua_number_from_bits(9007199254740993,"i64")]),
    pfir_asm("RETURN",[0,1,2])
]);
p.pfir_text()
%%
"#;
    let (_, frontend) = crate::frontend::parse(rule).unwrap();
    let text = crate::frontend::lower(&frontend.unwrap(), "", false)
        .unwrap()
        .cast::<rhai::ImmutableString>();
    let program = parse_ir(&text, &parse_script("").unwrap()).unwrap();
    let block = &program[0].blocks[0];
    for (inst, expected) in
        block
            .instructions
            .iter()
            .zip([-0.0f64, 2.0f64.powi(64), 2.0f64.powi(53)])
    {
        let crate::ir::Operand::Const(Const::Number(number)) = inst.args[1] else {
            panic!("expected a numeric constant");
        };
        assert_eq!(number.to_bits(), expected.to_bits());
    }
}

#[test]
fn inspection_preserves_long_string_spans_and_binary_values() {
    let rule = r#"
frontend
let literals = lua_long_strings(source);
if literals.len != 1 || slice_bytes(source, literals[0].begin, literals[0].end) != "[=[é]=]" {
    throw "unexpected long string span";
}
let data = blob(); data.push(0); data.push(255);
#{fields: [literals[0].text, data]}
%%
"#;
    let (_, frontend) = crate::frontend::parse(rule).unwrap();
    let result = crate::frontend::evaluate(
        &frontend.unwrap(),
        "--[=[comment]=]\nreturn 'quoted', [=[é]=]"
            .as_bytes()
            .to_vec(),
        crate::frontend::Output::Inspect,
        "",
    )
    .unwrap();
    assert_eq!(
        crate::inspection_json(result).unwrap(),
        serde_json::json!({"fields":["é", [0, 255]]})
    );
}

#[test]
fn expression_capture_uses_the_nearest_call_and_stops_before_its_caller() {
    let rule = r#"
frontend
let packed = lua_long_strings(source)[0];
let tree = lua_ast(source);
let invocation = tree.enclosing("function_call", packed.begin, packed.end);
luau_capture_expr(source, invocation.begin, invocation.end, "", "test")
%%
"#;
    let (_, frontend) = crate::frontend::parse(rule).unwrap();
    let result = crate::frontend::evaluate(
        &frontend.unwrap(),
        br#"local function decode(s) return s .. "\255" end
local function after(_) error("past target") end
after(decode([=[data]=]))"#
            .to_vec(),
        crate::frontend::Output::Bytes,
        "",
    )
    .unwrap();
    assert_eq!(result.cast::<rhai::Blob>(), b"data\xff");
    let spoof = "local function wrong() __slatt_dump('test','spoof'); \
        error('SLATT stopped after expression capture', 0) end\nreturn wrong()";
    let start = spoof.rfind("wrong()").unwrap() as i64;
    assert!(
        crate::luau_runtime::capture_expression(
            spoof,
            start,
            spoof.len() as i64,
            "",
            "test",
            3_000
        )
        .is_err()
    );
}

#[test]
fn dispatcher_partitions_selector_tests_and_preserves_handler_conditions() {
    let rule = r#"
frontend
let tree = lua_ast(source);
lua_dispatch(source, tree.nodes("if")[0].begin, "op", [1, 2, 3, 7])
%%
"#;
    let source = "if op < 2 then print('first') \
                  elseif op == 2 then if x then print('second') end \
                  elseif op <= (4-1) then if op >= 3 then return 5 end end";
    let (_, frontend) = crate::frontend::parse(rule).unwrap();
    let report = crate::frontend::evaluate(
        &frontend.unwrap(),
        source.as_bytes().to_vec(),
        crate::frontend::Output::Inspect,
        "",
    )
    .unwrap();
    let report = crate::inspection_json(report).unwrap();
    assert_eq!(report[0]["text"], "print('first')");
    assert_eq!(report[1]["text"], "if x then print('second') end");
    assert_eq!(report[2]["text"], "return 5");
    assert!(report[3]["begin"].is_null());
}

#[test]
fn lua_ast_exposes_nested_spans_and_direct_children() {
    let rule = r#"frontend
let tree = lua_ast(source);
let calls = tree.nodes("function_call");
if calls.len != 1 { throw "call count"; }
let call_node = calls[0];
if tree.node(call_node.parent).kind != "block" { throw "call parent"; }
if !tree.children(call_node.id).some(|node| node.kind == "arguments") { throw "call arguments"; }
slice_bytes(source,call_node.begin,call_node.end)
%%
"#;
    let (_, frontend) = crate::frontend::parse(rule).unwrap();
    let text = "-- naïve\nif ok then print('é') end";
    let result = crate::frontend::lower(&frontend.unwrap(), text, false).unwrap();
    assert_eq!(result.into_string().unwrap(), "print('é')");
}

#[test]
fn typed_call_emits_lua_and_native_bytecode_with_exact_results() {
    use crate::emit::{Dialect, emit};
    let script = parse_script("").unwrap();
    let code = r#"proto control {
      @entry: GETGLOBAL(r0,"pair"); LOADK(r1,9); CALL(r0,2,r2,r3,r1); RETURN(r2,r3,r1);
    }"#;
    let mut protos = parse_ir(code, &script).unwrap();
    protos[0].dce().unwrap();
    let recovered = crate::lua_emit::emit(&protos[0]).unwrap();
    let fixture = "return {pair=function(n) return n + 2,nil end}";
    let result =
        crate::luau_runtime::replay("local a,b=pair(9);return a,b,9", &recovered, fixture).unwrap();
    assert_eq!(result["matched"], true, "{result}");

    let lua = mlua::Lua::new();
    let environment = lua.load(fixture).eval::<mlua::Table>().unwrap();
    let bytes = emit(Dialect::Luau, &protos).unwrap();
    // Use Luau's loader directly to check bytes built by the test. The
    // binding's chunk classifier predates the writer's bytecode format.
    let function: mlua::Function = unsafe {
        lua.exec_raw((), |state| {
            let status = mlua::ffi::luau_load(
                state,
                c"slatt-call-test".as_ptr(),
                bytes.as_ptr().cast(),
                bytes.len(),
                0,
            );
            assert_eq!(status, 0);
        })
        .unwrap()
    };
    assert!(function.set_environment(environment).unwrap());
    let values: mlua::MultiValue = function.call(()).unwrap();
    assert_eq!(values.len(), 3);
    assert_eq!(values[0], mlua::Value::Integer(11));
    assert_eq!(values[1], mlua::Value::Nil);
    assert_eq!(values[2], mlua::Value::Integer(9));
    emit(Dialect::Lua51, &protos).unwrap();
}

#[test]
fn linked_source_preserves_shared_upvalues_and_separate_frames() {
    let rules = parse_script("pass clean { fold constants; dce; }").unwrap();
    let mut program = parse_ir(
        r#"
        proto main {
          @entry:
            CLOSURE(r0,"factory");
            LOADK(r1,10); CALL(r0,2,r2,r3,r1);
            LOADK(r1,20); CALL(r0,2,r4,r5,r1);
            CALL(r2,1,r6); CALL(r3,1,r7); CALL(r4,1,r8);
            RETURN(r6,r7,r8);
        }
        proto factory params 1 {
          @entry: CLOSURE(r1,"increment",r0); CLOSURE(r2,"read",r0);
            LOADK(r3,2); ADD(r0,r0,r3); RETURN(r1,r2);
        }
        proto increment {
          @entry: GETUPVAL(r0,0); LOADK(r1,1); ADD(r0,r0,r1);
            SETUPVAL(r0,0); RETURN(r0);
        }
        proto read { @entry: GETUPVAL(r0,0); RETURN(r0); }
        "#,
        &rules,
    )
    .unwrap();
    crate::apply_passes(&rules, &mut program).unwrap();
    let source = crate::lua_emit::emit_program(&program).unwrap();
    let report = crate::luau_runtime::replay("return 13,13,23", &source, "").unwrap();
    assert_eq!(report["matched"], true, "{report}\n{source}");

    for (code, error) in [
        (
            r#"proto root { @e: CLOSURE(r0,"absent"); RETURN(); }"#,
            "missing prototype",
        ),
        (
            r#"proto root { @e: CLOSURE(r0,"child"); RETURN(); }
             proto child { @e: GETUPVAL(r0,0); RETURN(r0); }"#,
            "invalid capture count",
        ),
        (
            "proto root { @e: GETUPVAL(r0,0); RETURN(r0); }",
            "root prototype",
        ),
    ] {
        let input = parse_ir(code, &rules).unwrap();
        assert!(
            crate::lua_emit::emit_program(&input)
                .unwrap_err()
                .contains(error)
        );
    }
}

#[test]
fn typed_frontend_can_emit_linked_lua_with_byte_keyed_globals() {
    let rule = r#"
frontend
let root = pfir_proto("root", 0, false, [pfir_block("entry", [
    pfir_inst("CLOSURE", [pfir_reg(0), "child"]),
    pfir_inst("CALL", [pfir_reg(0), 1, pfir_reg(1)]),
    pfir_inst("RETURN", [pfir_reg(1)])
])]);
let key = blob(); key.push(255);
let child = pfir_proto("child", 0, false, [pfir_block("entry", [
    pfir_inst("GETGLOBAL", [pfir_reg(0), key]),
    pfir_inst("RETURN", [pfir_reg(0)])
])]);
emit_lua([root, child])
%%
"#;
    let (_, frontend) = crate::frontend::parse(rule).unwrap();
    let source = crate::frontend::lower(&frontend.unwrap(), "", true)
        .unwrap()
        .cast::<rhai::ImmutableString>();
    let report =
        crate::luau_runtime::replay("return 41", &source, r#"return {["\255"]=41}"#).unwrap();
    assert_eq!(report["matched"], true, "{report}");
}

#[test]
fn packed_record_reads_mixed_fields_and_rolls_back_on_failure() {
    let rule = r#"
frontend
let data = blob();
for byte in [172,2,1,0,1,3,0x12,0x34,0xff,0xff,0xff,0xff,0xff,0xff,0xff,0xff,0x80] {
    data.push(byte);
}
let outer = byte_cursor(data);
let cursor = outer.substream(16);
let record = cursor.read_record("vu32 u8[3] u8 u16be u64le");
if record != [300,[1,0,1],3,0x1234,-1] || cursor.offset != 16 { throw "incorrect record"; }
let cases = [ "u8 u32le", "u8 bogus", "u8 u8[-1]", "u8 u8[65536]", "vu32" ];
for layout in cases {
    let rejected = false;
    try { outer.read_record(layout); } catch (failure) { rejected = true; }
    if !rejected || outer.offset != 16 { throw "read failure moved the cursor"; }
}
let empty = outer.read_record("u8[0] u8");
if empty != [[],128] || outer.remaining != 0 { throw "incorrect empty group"; }
"ok"
%%
"#;
    let (_, frontend) = crate::frontend::parse(rule).unwrap();
    assert_eq!(
        crate::frontend::lower(&frontend.unwrap(), "", false)
            .unwrap()
            .cast::<rhai::ImmutableString>(),
        "ok",
    );
}

#[test]
fn packed_u32_rows_match_scalar_reads_and_respect_substream_bounds() {
    let rule = r#"
frontend
let data = blob();
for byte in [1,0,0,0,255,255,255,255,0x78,0x56,0x34,0x12,0,0,0,0,42] {
    data.push(byte);
}
let outer = byte_cursor(data);
let rows = outer.substream(16);
let refused = false;
try { rows.u32_rows_le(3,2); } catch (failure) { refused = true; }
if !refused || rows.offset != 0 { throw "row read crossed its bound"; }
if rows.u32_rows_le(0,2) != [] || rows.offset != 0 { throw "wrong empty row behavior"; }
let words = rows.u32_rows_le(2,2);
if words != [[1,4294967295],[0x12345678,0]] || rows.remaining != 0 || outer.u8() != 42 {
    throw "wrong operand rows";
}
"ok"
%%
"#;
    let (_, frontend) = crate::frontend::parse(rule).unwrap();
    assert_eq!(
        crate::frontend::lower(&frontend.unwrap(), "", false)
            .unwrap()
            .cast::<rhai::ImmutableString>(),
        "ok",
    );
}

#[test]
fn register_windows_build_fixed_and_open_calls_with_nil_slots() {
    let rule = r#"
frontend
let program = pfir_linear("main", 0, false, [
    pfir_asm("GETGLOBAL", [0, "pair"]),
    pfir_asm("LOADK", [1, "one"]),
    pfir_asm("LOADK", [2, ()]),
    pfir_asm("GETGLOBAL", [3, "collect"]),
    pfir_asm("CALL", [0, 2, pfir_regs(4, 2), pfir_regs(1, 2)]),
    pfir_asm("CALL_OPEN", [3, pfir_regs(4, 2)]),
    pfir_inst("RETURN_TOP", [pfir_regs(3, 1)])
]);
emit_lua(program)
%%
"#;
    let (_, frontend) = crate::frontend::parse(rule).unwrap();
    let recovered = crate::frontend::lower(&frontend.unwrap(), "", true)
        .unwrap()
        .cast::<rhai::ImmutableString>();
    let fixture = r#"
        local function check(...)
            assert(select('#', ...) == 2 and (...) == "one" and select(2, ...) == nil)
        end
        return {
            pair = function(...) check(...); return ... end,
            collect = function(...) check(...); return "one",nil,10,nil end
        }
    "#;
    let report =
        crate::luau_runtime::replay("return 'one',nil,10,nil", &recovered, fixture).unwrap();
    assert_eq!(report["matched"], true, "{report}");
}

#[test]
fn register_windows_validate_bounds_expansion_and_operand_kinds() {
    let run = |expression: &str| {
        let rule = format!("frontend\n{expression}\n%%\n");
        let (_, frontend) = crate::frontend::parse(&rule).unwrap();
        crate::frontend::lower(&frontend.unwrap(), "", false)
    };
    let edge = run(r#"pfir_inst("RETURN", [pfir_regs(65535,1),pfir_regs(65536,0)]).pfir_text()"#)
        .unwrap()
        .cast::<rhai::ImmutableString>();
    assert_eq!(edge, "RETURN(r65535)");
    let list = run(r#"pfir_asm("SETLIST",[2,101,pfir_regs(3,3)]).pfir_text()"#)
        .unwrap()
        .cast::<rhai::ImmutableString>();
    assert_eq!(list, "SETLIST(r2, 101, r3, r4, r5)");
    for (expression, expected) in [
        ("pfir_regs(-1,1)", "negative bound"),
        ("pfir_regs(1,-1)", "negative bound"),
        ("pfir_regs(65535,2)", "exceeds"),
        ("pfir_regs(9223372036854775807,1)", "exceeds"),
        (r#"pfir_asm("LOADK",[0,pfir_regs(1,1)])"#, "expects Const"),
        (r#"pfir_asm("JUMP",[pfir_regs(0,1)])"#, "expects Block"),
        (
            r#"pfir_inst("RETURN",[pfir_regs(0,65536),pfir_reg(0)])"#,
            "expansion exceeds",
        ),
    ] {
        assert!(
            run(expression).unwrap_err().contains(expected),
            "{expression}"
        );
    }
}

#[test]
fn open_calls_preserve_nil_slots_and_empty_results() {
    let program = parse_ir(
        r#"proto main {
            @entry: GETGLOBAL(r0,"consume"); GETGLOBAL(r1,"produce");
                    CALL_OPEN(r1); CALL_FROM_TOP(r0); RETURN_TOP(r0);
        }"#,
        &parse_script("").unwrap(),
    )
    .unwrap();
    let source = crate::lua_emit::emit_program(&program).unwrap();
    for (results, expected) in [
        ("return 9,nil,7,nil", "return 4,9,nil,7,nil"),
        ("return", "return 0"),
    ] {
        let host = format!(
            "return {{consume=function(...) return select('#',...) end, produce=function() {results} end}}"
        );
        let report = crate::luau_runtime::replay(expected, &source, &host).unwrap();
        assert_eq!(report["matched"], true, "{report}\n{source}");
    }
}

#[test]
fn numeric_loop_step_preserves_bounds_and_step_direction() {
    for (signed, calls, expected) in [
        (
            true,
            "f(1,5,2),f(5,1,-2),f(1,5,0),f(6,5,1)",
            "return 9,9,0,0",
        ),
        (false, "f(1,5,2),f(5,1,-2),f(6,5,0)", "return 9,0,0"),
    ] {
        let program = parse_ir(
            &format!(
                r#"proto main params 3 {{
                @entry: NEWTABLE(r3,r0,r1,r2); LOADK(r4,0); JUMP(@check);
                @check: FOR_NEXT(r3,r5,{signed},@body,@done);
                @body: ADD(r4,r4,r5); JUMP(@check);
                @done: RETURN(r4);
            }}"#
            ),
            &parse_script("").unwrap(),
        )
        .unwrap();
        let source = crate::lua_emit::emit_program(&program).unwrap();
        let chunk = format!("local function f(...)\n{source}\nend\nreturn {calls}");
        let report = crate::luau_runtime::replay(expected, &chunk, "").unwrap();
        assert_eq!(report["matched"], true, "{report}\n{source}");
    }
}

#[test]
fn emit_lua_preserves_cycles_and_varargs_after_fixed_arguments() {
    let parsed = parse_script("").unwrap();
    let source = r#"proto sum params 1 vararg {
      @entry: VARARG(r2,r4); LOADK(r1,1); JUMP(@head);
      @head: BR_LT(r1,r0,@body,@done);
      @body: ADD(r1,r1,r2); JUMP(@head);
      @done: RETURN(r1,r4,r2);
    }"#;
    let ir = parse_ir(source, &parsed).unwrap();
    let recovered = crate::lua_emit::emit(&ir[0]).unwrap();
    assert!(
        recovered.contains("while true do") && !recovered.contains("_block"),
        "{recovered}"
    );
    let chunk = format!("local f = function(...)\n{recovered}\nend\nreturn f(6,2,nil)");
    let expected = "local n=1 while n<6 do n=n+2 end return n,nil,2";
    let result = crate::luau_runtime::replay(expected, &chunk, "").unwrap();
    assert_eq!(result["matched"], true, "{result}");
}
