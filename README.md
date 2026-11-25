# Slatt

<p align="center">
  <img src="slatt.png" alt="Slatt logo" width="160">
</p>

Slatt is a standalone language for writing bytecode lifting rules and
instruction passes. A `.slatt` file defines opcode maps and passes. A `.pfir`
file contains typed prototypes and basic blocks. Slatt can import compiled
Lua 5.1 and Luau chunks or lift raw instructions through a user-defined map.
It runs the passes in declaration order and prints normalized `.pfir`.

A `.slatt` file can begin with a Rhai-based `frontend` section terminated by
`%%` on its own line. The section receives raw `input_bytes` and UTF-8
`source` when available. It can return typed PFIR, PFIR text, a byte blob,
Lua source, or an inspection report depending on the command. The rest of
the file defines Slatt passes.
The [MoonSec example](examples/moonsec/README.md) starts directly from protected
Lua source.

## Split a project across files

Put one `include "relative/path.slatt";` directive per line at the top of a
rules file. Relative paths resolve from the file that declares them. Dependencies
load before the importing file, in directive order. A file shared by two
dependencies loads once. Cycles and duplicate pass, map, or frontend function
definitions across files are errors.

Each file can define frontend helpers, maps, or passes. Frontend sections share
one Rhai scope and execute in include order. Put the final result in the entry
file. `run` applies the collected bytecode passes in order. `recover` writes
Lua source from the frontend or applies the passes and emits Lua from typed PFIR.

The example keeps grammar parsing, AST construction, opcode mapping, and
passes in separate files:

~~~powershell
slatt check examples/project/main.slatt
slatt run examples/project/main.slatt examples/project/input.lua --emit lua51 -o mix.luac
slatt recover examples/project/main.slatt examples/project/input.lua -o mix.lua
~~~

For unsaved entry contents, use `slatt check - --stdin-path path/to/main.slatt`.
Included files are read from disk.

## Inspect source and trace Luau

```powershell
slatt trace input.luau --fixture host.luau
slatt replay original.luau recovered.lua --fixture host.luau
slatt inspect examples/snowveil.slatt input.luau
slatt inspect examples/snowveil-payload.slatt input.luau --fixture host.luau
```

`trace` runs text in a fresh embedded Luau state and writes a JSON report.
It records fallback global lookups, missing globals, first writes to absent
global keys, `print` calls, explicit `__slatt_probe(label, ...)` observations,
and the returned tuple or runtime error. Repeated lookups are counted by key,
line, and availability. The fixture is optional Luau source
that returns a table of host bindings. The runner has an allowlist of Luau
standard libraries, memory and execution limits, and bounded report sizes.
Raw table access and updates of existing global keys bypass these hooks.
Snapshots capture scalar values and bounded table contents.

`replay` runs both files in fresh states with the same fixture. It compares
scalar return slots, ordered print and probe events, and named byte captures.
A mismatch or unsupported observation yields `matched: false` and exit status 1.
Replay covers those observations for the supplied fixture. Global lookup order,
raw writes, and other unobserved effects are outside the comparison.

`inspect` serializes the frontend result as JSON. A rule can pass code,
`host_fixture`, and a chunk name to `luau_trace` and inspect its report.
`replace_once` patches a unique source anchor. `token_span(index)` on a
`lua_token_buffer` returns zero-based byte offsets, and `splice_bytes`
edits that half-open range in the original source.

For packed data, `byte_cursor(blob)` exposes `read_uint(width, big_endian)`,
`read_varuint()`, `take(size)`, and `substream(size)`, with `offset` and
`remaining` properties. Child streams are bounded by their own region and
keep offsets relative to the original blob. An eight-byte integer is returned
as a signed bit pattern. The varuint reader supports up to 63 bits. Blobs
also expose `adler32()` and `decode_base85(alphabet, big_endian)` for complete
Base85 words with a caller-provided alphabet.

Use `u8()`, `u16le()`, `u32le()`, `u64le()`, `vu32()`, and
`u32s_le(count)` for common packed fields. For an opcode followed by
three flags and a mode, use `cursor.read_record("vu32 u8[3] u8")`.
It returns `[opcode, flags, mode]`, with repeated fields grouped into
one array. Layouts accept `u8`, fixed 16-, 32-, and 64-bit unsigned words
with `le` or `be` suffixes, and `vu32` or `vu63`. A failed read leaves
the cursor at its original offset.
`cursor.u32_rows_le(rows, width)` reads an array of fixed-width operand
rows in one call, with the same cursor bounds.

`decode_ascii85()` accepts unpadded partial words made from digits
`!` through `u`.
`bit_cursor_lsb(blob)` reads fixed fields, Elias gamma, Rice values and
Huffman codes from least-significant-bit-first bytes. Build a table with
`lengths.canonical_huffman()`, where byte index is the symbol and zero
excludes that symbol, or `prefix_huffman(entries)` with explicit
`[symbol, width, code]` entries. `read_huffman(table)` consumes one code.
`read_mtf_runs(table, length, escape)` handles gamma-coded rank runs.
`append_byte(byte, count)` and `decode_mtf(context_bits)` handle other run
layouts. The context uses high bits of the previous decoded byte, with zero
selecting one shared move-to-front list. `column.inverse_bwt(row)` restores
a Burrows–Wheeler block.

## Lift a packed VM with Slatt

Run these from the Slatt directory with fresh output paths.

```powershell
slatt unpack examples/workflow/fixture.slatt -o packed.bin
slatt run examples/workflow/lift.slatt packed.bin
slatt inspect examples/workflow/lift.slatt packed.bin
slatt recover examples/workflow/lift.slatt packed.bin -o recovered.lua
slatt inspect examples/workflow/verify.slatt packed.bin
```

The fixture runs a small Luau chunk inside Slatt. `__slatt_dump(label, value)`
captures a string or buffer verbatim. `luau_capture(code, fixture, name)`
returns a report and the captured blobs under `artifacts`. `unpack` writes
the blob returned by the frontend to a new file.

The lifter decodes the packed blocks with a byte cursor and constructs PFIR
with `pfir_reg`, `pfir_target`, `pfir_inst`, `pfir_block` and `pfir_proto`.
`cfg()` exposes predecessors, successors and reachability.
`lua_ast(source)` exposes nodes by kind, direct children and byte spans.
`lua_long_strings(source)` selects bracketed literals and exposes their
text and byte spans. `inspect` serializes blobs as JSON byte arrays.
`lua_token_buffer(source).find_tokens(pattern, start)` searches for a
contiguous sequence of syntax tokens and returns its index or `-1`.
The verifier uses the AST and calls `luau_replay(original, recovered, fixture)`
for true, false and nil branch inputs.

When a decoder returns a flat stream, pass its typed instructions to
`pfir_linear(name, params, variadic, instructions)`. Use `pfir_pc(index)`
for zero-based jump targets. The builder splits blocks at targets and
terminators, adds fallthrough jumps, and rejects invalid targets.
An array of instructions in one stream slot represents one physical
instruction that expands into several PFIR operations. Branch targets
still count physical slots. A bundle may terminate only at its last operation.
`pfir_jump_table(register, count)` constructs a computed jump with one
destination per zero-based physical PC. The CFG includes every listed
destination.
`pfir_asm("BR_TRUE", [0, 3, 4])` uses the opcode signature to interpret
integers as a register and two PCs. For `pfir_asm("LOADK", [0, 42])`,
the second integer stays a constant.
Use `pfir_regs(first, count)` inside a `pfir_asm` or `pfir_inst` operand
list to expand a contiguous window into typed registers. For example,
`pfir_asm("CALL", [base, 2, pfir_regs(base, 2), pfir_regs(base + 1, argc)])`
writes two results starting at `base` and reads `argc` arguments starting
at the next register. Empty windows expand to zero operands. Windows
reject negative or overflowing bounds before building the instruction.

`lua_number_from_bits(word, encoding)` decodes an `f64` bit pattern or
converts an `i64` or `u64` word to a Lua number with double-precision rounding.

`lua_dispatch(source, if_begin, selector, opcodes)` follows integer
selector tests for each supplied opcode and returns source spans and
handler text. It can descend through nested selector tests and preserves
conditions that depend on VM state inside the handler.

Match complete handler bodies with token templates. An identifier starting
with `__match_` binds one token. Repeating the identifier requires the
same token. A mismatch returns unit.

```text
let move = lua_token_buffer("regs[word[__match_dst]]=regs[word[__match_src]]");
let body = lua_token_buffer(handler.text);
let fields = body.match_pattern(move);
```

When using `lua_dispatch`, match directly inside the whole-source token
buffer with `tokens.match_span(handler.begin, handler.end, patterns)`. Pass
an array of token buffers in priority order. The result is
`[zero_based_pattern_index, bindings]` or unit when none match. Both byte
offsets must align with the first and last token in the handler.

`slatt run … --emit lua -o output.lua` and
`slatt emit lua portable.pfir output.lua` emit source for a portable
program. The first prototype is the entry. Frontends can return an ordered
array of `pfir_proto` values or call `emit_lua` on that array.
The writer structures acyclic branches and simple natural loops
and uses a dispatcher for other control-flow graphs. Opaque native
instructions require lifting to typed operations before source emission.

### Link closures

```text
proto main {
  @entry:
    LOADK(r0, 42);
    CLOSURE(r1, "read", r0);
    CALL(r1, 1, r2);
    RETURN(r2);
}
proto read {
  @entry:
    GETUPVAL(r0, 0);
    RETURN(r0);
}
```

`CLOSURE` captures registers from the current activation by reference.
Use a zero-based numeric slot in place of a register to forward an inherited
capture. In a frontend, `pfir_upvalue(slot)` creates that operand. Linked
closures share cell updates through both kinds of capture.
`CLOSEUPVALS(rN)` detaches open local captures at registers `N` and above.
A later capture of the same register starts a new cell. Existing closures
continue sharing their old cell.
`GETUPVAL` reads and `SETUPVAL` writes a zero-based capture slot. The
linker checks target names, inherited slots, and capture counts. These
operations currently emit Lua source.

For an obfuscated input from a supported family, select the corresponding
rule file. The audited IronBrew2 profile decodes its image into typed PFIR.
The IronVeil profile reconstructs seven prototypes through its family-specific
source frontend.

```powershell
slatt run examples/ironbrew2.slatt protected.lua
slatt recover examples/ironbrew2.slatt protected.lua -o recovered.lua
slatt replay protected.lua recovered.lua
```

For IronBrew1, start with static inspection of the packed VM.

```powershell
slatt inspect examples/ironbrew1.slatt protected.lua > vm.json
slatt unpack examples/ironbrew1.slatt protected.lua -o vm.bin
slatt unpack examples/ironbrew1-capture.slatt protected.lua -o observed.bin
```

The report includes the packed literal span, image hash, prototype tree,
constants, capture descriptors, opcode counts, and raw instructions by PC.
The rule discovers the packed literal from Lua syntax. `unpack` extracts its
decompressed image, and `inspect` also validates the prototype layout.
Opcode numbers and operand groups are reported as encoded.
`recover` requires the audited wrapper and payload graph.

The capture rule uses `tree.enclosing("function_call", begin, end)` and
`luau_capture_expr(source, begin, end, fixture, label)` to record the
original unpacker's bytes in the embedded Luau runtime. It stops at the
selected call's result and gives you an independent byte image to compare
with static unpacking. The optional sixth argument is a timeout in
milliseconds, from 1 through 30,000. The default is 3,000.

For the audited Rice dispatcher in corpus sample `6e13b922aaee6e0a.lua`,
`ironbrew1-rice.slatt` maps handler templates to PFIR and reports each
unresolved PC with its reason.

```powershell
slatt inspect examples/ironbrew1-rice.slatt input.lua > typed-sites.json
slatt recover examples/ironbrew1-rice.slatt input.lua -o child-factory.lua
```

The recovered chunk returns the factory for prototype `0.2.0`. Pass the
captured value to the factory, then call the returned function. The report
covers the entire prototype tree and shows why any site remains unresolved.

To select another fully lifted prototype subtree, include the audited rule
and call `export_child` with its path.

```text
include "ironbrew1-rice.slatt";

frontend
export_child(root, plans, "0.2.3")
%%
```

Use the full-tree entry point to lift the linked program with the same rules.

```powershell
slatt run examples/ironbrew1-rice-full.slatt input.lua --emit lua -o lifted.lua
```

This entry point checks every physical instruction. The Lua emitter uses a
dispatcher for indirect branches and VM-specific loop steps.

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

## Release builds

Version tags trigger the **Windows release** workflow, which builds the CLI
ZIP and VSIX on a GitHub-hosted runner. The workflow also accepts an existing
tag for a test build and stores the resulting packages as Actions artifacts.
Enable **Publish** on the manual run to upload them to GitHub Releases.

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
The default output is analysis IR. The importer reads the chunk without executing it.
[Lua 5.1 format](https://lua.org/source/5.1/lundump.c.html) · [Luau format](https://github.com/luau-lang/luau/blob/0.748/Common/include/Luau/Bytecode.h)

## Emit runnable bytecode

```powershell
slatt run examples/simplify.slatt input.luac --emit lua51 -o output.luac
slatt emit luau portable.pfir output.luauc
```

The targets write little-endian Lua 5.1 chunks with 64-bit `size_t` and Luau
v14 chunks. The emitter resolves branch labels, allocates scratch registers
for nonconsecutive fixed varargs or returns, and checks its output by reading
it back. It creates a new file and refuses to overwrite an existing one.

Use `params N` after the prototype name to declare fixed parameters and
`vararg` to allow fixed `VARARG(r0, …)` reads. Portable Luau emission supports
one prototype with typed instructions. Direct Lua 5.1 input can retain
its original prototype tree, constant pool, and unchanged native operations.
Keep those transformations in one `run … --emit lua51` command so the binary
input supplies the template. `XOR32` requires a known result at emission.

The [custom-VM example](examples/deobfuscator) maps permuted operands and
opcodes, removes an unreachable decoy, and emits both formats. The
[MoonSec example](examples/moonsec) uses a static MoonSec decoder before
Slatt threads jump chains, simplifies the graph, and emits Lua 5.1.

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

`thread jumps` redirects typed branches through blocks that only jump.
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
| `NOP` | none, preserves a physical PC without emitting an operation |
| `MOVE` | destination register, source register |
| `ADD`, `SUB`, `MUL`, `DIV` | destination register, left register, right register |
| `GETGLOBAL` | destination register, global name as a byte string |
| `CALL` | callee register, result count, result registers, argument registers |
| `CALL_OPEN` | callee register, argument registers. Writes all results starting at the callee and sets the result top |
| `CALL_FROM_TOP` | callee register. Calls with the following registers through the result top and stores the first result |
| `TAILCALL` | callee register and argument registers, with all results forwarded |
| `CLOSURE` | destination register, prototype name, capture registers or inherited upvalue slots |
| `CLOSEUPVALS` | first register whose open capture is detached |
| `GETUPVAL` | destination register, zero-based capture slot |
| `SETUPVAL` | source register, zero-based capture slot |
| `NEWTABLE` | destination register, optional array element registers |
| `GETTABLE` | destination register, table register, key register |
| `SETTABLE` | table register, key register, value register |
| `SELF` | method destination, receiver destination, object register, key register |
| `SETLIST` | table register, first integer key, value registers |
| `NOT`, `LEN` | destination register, source register |
| `POW`, `CONCAT`, `EQ`, `NE`, `LT` | destination register, left register, right register |
| `XOR32` | destination register, left register, right register |
| `JUMP` | destination block |
| `JUMP_TABLE` | selector register, ordered destinations |
| `FOR_NEXT` | loop state table register, iterator destination, signed-step flag, body block, exit block |
| `BR_TRUE` | test register, true block, false block |
| `BR_EQ`, `BR_LT`, `BR_LE` | left register, right register, true block, false block |
| `VARARG` | zero or more destination registers in a variadic prototype |
| `RETURN` | zero or more result registers |
| `RETURN_TOP` | first result register. Returns through the result top |

`BR_TRUE` uses Lua truthiness. `XOR32` folds exact unsigned 32-bit operands.
Structural validation checks operand kinds and successors after each rewrite.
Rule authors supply the conditions that make their transformations equivalent.
The table, unary, power, concatenation and comparison-value operations
listed above support Lua source emission. `JUMP_TABLE` uses Lua addition
to turn a zero-based selector into the corresponding one-based table key.
The emitted code raises an error when that key has no destination.
Large fallback dispatchers test the first four blocks directly and split
the remaining range before testing individual blocks. Long contiguous jump
tables use a compact initializer. Selector
arithmetic, destination ordering, and invalid-key behavior stay the same.
The Lua emitter starts the result top at −1 and preserves nil slots in
open calls and returns. `FOR_NEXT` takes a table containing current value,
limit, and step. With the signed-step flag set, zero steps exit and negative
steps use a descending bound. With the flag clear, the comparison is always
ascending.

The binary regression fixtures were compiled from `examples/branch.lua` and
`tests/fixtures/native.lua` with the workspace Lua 5.1 and Luau compilers and
the Luau 0.748 release compiler. The 0.748 fixtures use `--binary -O0 -g2` and
`--binary -O2 -g1`, with the latter exercising fast calls and protected calls.
