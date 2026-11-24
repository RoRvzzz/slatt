# MoonSec V3 source deobfuscator

```powershell
slatt recover examples/moonsec/source.slatt protected.lua -o recovered.lua
slatt run examples/moonsec/source.slatt protected.lua --emit lua51 -o recovered.luac
```

The source target emits the audited V3 comparison and call as ordinary Lua.
It keeps the wrapper's banner callback and global effects. The frontend
receives `source_output` as true during `slatt recover`. The bytecode passes
run on the `slatt run` route.

The frontend section of `source.slatt` locates the encoded stream in parsed Lua
tokens, recovers its byte offset, reads the constant pool and instruction records,
checks the supported dispatcher templates, and lifts their handlers. Slatt then
imports that native instruction stream, threads jumps, prunes blocks, and emits
Lua 5.1 bytecode. Omit `--emit lua51 -o …` to inspect the lifted IR.

Coverage starts with the V3 dispatcher in `c82b007d8770fd97.lua`, including its
fused comparison and call handlers. Unknown dispatcher templates, operand forms,
child prototypes, and opcodes are rejected. Lua input is parsed as data. The
source route uses only the Slatt executable.

## Legacy comparison pipeline

The input adapter runs an existing static MoonSec frontend to decode and
classify the virtual instructions. Slatt imports that prototype tree,
runs `recover.slatt`, and writes the final Lua 5.1 bytecode. The Slatt pass
folds constants, removes self-moves, threads jump chains, prunes unreachable
blocks, and keeps the original constant indices and closure captures.
[MoonsecDeobfuscator](https://github.com/tupsutumppu/MoonsecDeobfuscator)

From the containing Luraph repository, use its wrapper-aware frontend to retain
MoonSec's entry initialization:

```powershell
.\tools\slatt\examples\moonsec\deobfuscate.ps1 `
  -InputFile .\protected.lua -OutputFile .\recovered.luac `
  -Frontend .\target-family-cli\release\deob-moonsec.exe
```

`-Slatt` can point to a specific build. The adapter also accepts a frontend DLL
with the upstream `-dev -i … -o …` command line. Output coverage depends on
which input handlers that frontend can decode.

To inspect the Slatt pass on an already decoded chunk, run
`slatt run recover.slatt decoded.luac` for textual IR. Use `--emit lua51 -o …`
on that binary input to keep the prototype template during emission.
