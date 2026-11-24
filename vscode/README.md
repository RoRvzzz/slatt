# Slatt for VS Code

The extension provides syntax highlighting and completion for Slatt rule files
(`.slatt`, `.slat`) and normalized IR (`.pfir`). Rule files also get live errors
from the Slatt compiler as you edit.

Install the VSIX from the Slatt GitHub release using **Extensions → Install from
VSIX**. Put `slatt` on `PATH` or set **Slatt: Executable Path** to the compiler.
Open a rule file and type `match-eq` to insert a two-arm equality rule.

Diagnostics run in trusted workspaces. Highlighting and completion also work in
Restricted Mode.
