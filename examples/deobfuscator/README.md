# Custom-VM deobfuscator

`obfuscated.pfir` describes a small VM with permuted opcode IDs and operands.
`deobfuscate.slatt` maps the instructions, folds an XOR-based opaque predicate,
removes its decoy branch, and keeps a conditional branch on the runtime input.

```powershell
slatt run deobfuscate.slatt obfuscated.pfir --emit lua51 -o recovered.luac
slatt run deobfuscate.slatt obfuscated.pfir --emit luau -o recovered.luauc
```

With the argument `7`, the recovered chunk returns `7, "low", nil`. With
`10`, it returns `20, "high", nil`. Both return tuples include the final `nil`.
