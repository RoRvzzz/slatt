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
