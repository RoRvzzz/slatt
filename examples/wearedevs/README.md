# WeAreDevs offline recovery

Run from the extracted bundle directory.

~~~powershell
.\slatt.exe recover .\wearedevs\main.slatt .\obfuscated.lua -o .\recovered.lua
~~~

The bundle contains the published Windows Slatt v0.3.0 executable and six
`.slatt` source files. Recovery runs offline with those files. The rules
inspect Lua tokens without executing the input.

The audited literal-print template emits the application call on its own.
Its original wrapper behavior was outside the payload check.

Other supported single-line v1.0.0 wrappers produce Lua with a
`WeAreDevs structural lift` header. `dispatch.slatt` separates the VM selector
from the handler blocks and folds the selector thresholds. `bootstrap.slatt`
replaces certified reordered Base64 and mixed Base64/Base85 pools with Lua
literals. A changed bootstrap stays in the structural output. This output
still contains the VM loop and its register and closure state.

The shipped v0.3.0 binary has a 1 MiB input limit and a 65,536-entry token
array limit. Build the local CLI from `tools/slatt` for Luau syntax and inputs
up to 4 MiB. Both binaries use the same rule files.

~~~powershell
cargo build --release --offline --locked --manifest-path tools/slatt/Cargo.toml
~~~
