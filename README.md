# Slatt

Slatt is a standalone language for writing bytecode lifting rules and
instruction passes. A `.slatt` file defines opcode maps and passes. A `.pfir`
file contains typed prototypes and basic blocks. Slatt can import compiled
Lua 5.1 and Luau chunks or lift raw instructions through a user-defined map.
It runs the passes in declaration order and prints normalized `.pfir`.

## Run the example

From this directory:

```powershell
cargo run --offline --locked -- check examples/demo.slatt
cargo run --offline --locked -- run examples/demo.slatt examples/demo.pfir > demo.normalized.pfir
cargo run --offline --locked -- facts demo.normalized.pfir demo entry 4
```

The example imports a custom instruction whose constant and register operands
are reversed, folds two XOR instructions, and resolves the conditional jump.
The facts command reports constants known before instruction 4 in `@entry`.

## VS Code

Install the Slatt VSIX from the GitHub release. It adds syntax highlighting,
opcode and keyword completion, and snippets for `.slatt` files. The extension
also highlights `.pfir` files. For live rule diagnostics, put `slatt` on `PATH`
or set **Slatt: Executable Path**. The extension uses `slatt check -` on the
current editor buffer. [Releases](https://github.com/RoRvzzz/slatt/releases)

## Work with compiled bytecode

```powershell
slatt import input.luac
slatt run examples/simplify.slatt input.luauc > simplified.pfir
slatt facts simplified.pfir main pc0 0
```

Lua 5.1 import supports 32-bit instructions, 32- or 64-bit `size_t`, and
double-precision numbers in either byte order. Luau import supports bytecode
versions 6–14, including the v14 chunks emitted by Luau 0.748. Imports use
zero-based native PCs for block names and include child prototypes. Calls,
closure creation, and other VM operations outside the typed subset appear as
opaque `NATIVE` records. Opaque branches list their successors explicitly.
The output is analysis IR. The importer reads the chunk without executing it.
[Lua 5.1 format](https://lua.org/source/5.1/lundump.c.html) · [Luau format](https://github.com/luau-lang/luau/blob/0.748/Common/include/Luau/Bytecode.h)

## Write a pass

```text
pass simplify {
  fold constants;

  match [BR_EQ($left, $right, $yes, $no)]
    where const($left) == const($right) => [JUMP($yes)],
    where const($left) != const($right) => [JUMP($no)];

  prune unreachable;
  dce;
}
```

An instruction pattern matches a contiguous sequence within one block. A `$name`
binds a typed operand, and repeated uses in the pattern must match the same
operand. The replacement can use those bindings and literal operands. An empty
replacement deletes the matched sequence. Each rule scans the prototype once
and resumes after its replacement. Arms are tried in order and the first
matching arm wins.

`const($name)` returns a bound constant or the known value of a bound register
before the first matched instruction. A comparison with an unknown operand
skips the arm. `known($name)` is available for explicit presence tests.
Guards support `==`, `!=`, numeric `<`, `<=`, `>`, `>=`, and short-circuiting `and`.
Facts start unknown at entry and retain constants that agree across reachable
incoming paths. `fold constants` uses those facts to simplify moves and
arithmetic.

`prune unreachable` removes blocks with no path from entry in the rewritten
CFG. `dce` removes unused loads, moves, and arithmetic proved pure by the
constant analysis. Native effects are preserved. Prototypes that create
native closures keep register writes conservatively because a closure can
expose a captured register after it returns.

## Define an opcode map

```text
map demo_vm {
  41 => LOADK($1, $0);
  99 => RETURN($0);
}
```

```text
raw proto demo using demo_vm {
  @entry:
    41("hello", r0);
    99(r0);
}
```

Mapping slots use zero-based operand indices. Omitted indices are ignored.
Slatt checks the mapped operands against the target opcode's types and
reports missing or unmapped slots with the input location. A normalized
prototype uses `proto demo` and named opcodes directly.

## IR reference

The first block is the entry. `Reg` operands are spelled `r0` through `r65535`.
Block operands use `@name`. Constants include double-precision numbers,
byte strings, booleans, and `nil`. Slatt preserves non-UTF-8 bytes with `\xHH`
and nonfinite number encodings with `bits("…")`. Each block has one final control-flow
instruction, and every destination must exist in the same prototype.

| Opcode | Operands |
| --- | --- |
| `LOADK` | destination register, constant |
| `MOVE` | destination register, source register |
| `ADD`, `SUB`, `MUL`, `DIV` | destination register, left register, right register |
| `XOR32` | destination register, left register, right register |
| `JUMP` | destination block |
| `BR_TRUE` | test register, true block, false block |
| `BR_EQ`, `BR_LT`, `BR_LE` | left register, right register, true block, false block |
| `RETURN` | zero or more result registers |

`BR_TRUE` uses Lua truthiness. `XOR32` folds exact unsigned 32-bit operands.
Structural validation checks operand kinds and successors after each rewrite.
Rule authors supply the conditions that make their transformations equivalent.

The binary regression fixtures were compiled from `examples/branch.lua` and
`tests/fixtures/native.lua` with the workspace Lua 5.1 and Luau compilers and
the Luau 0.748 release compiler. The 0.748 fixtures use `--binary -O0 -g2` and
`--binary -O2 -g1`, with the latter exercising fast calls and protected calls.
