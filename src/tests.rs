use crate::ir::{Const, Op, Reg};
use crate::parser::{parse_ir, parse_script};

const RULES: &str = include_str!("../examples/demo.slatt");
const DEMO: &str = include_str!("../examples/demo.pfir");
const SIMPLIFY: &str = include_str!("../examples/simplify.slatt");

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
