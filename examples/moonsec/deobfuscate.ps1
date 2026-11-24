param(
    [Parameter(Mandatory)][string] $InputFile,
    [Parameter(Mandatory)][string] $OutputFile,
    [Parameter(Mandatory)][string] $Frontend,
    [string] $Slatt = 'slatt'
)

$ErrorActionPreference = 'Stop'
$source = (Resolve-Path -LiteralPath $InputFile).Path
$decoder = (Resolve-Path -LiteralPath $Frontend).Path
$rules = Join-Path $PSScriptRoot 'recover.slatt'
$workspace = Join-Path ([IO.Path]::GetTempPath()) ('slatt-moonsec-' + [guid]::NewGuid().ToString('N'))
[void][IO.Directory]::CreateDirectory($workspace)
$intermediate = Join-Path $workspace 'decoded.luac'
try {
    if ([IO.Path]::GetExtension($decoder) -eq '.dll') {
        & dotnet $decoder -dev -i $source -o $intermediate
    } else {
        & $decoder -dev -i $source -o $intermediate
    }
    if ($LASTEXITCODE -ne 0 -or -not (Test-Path -LiteralPath $intermediate -PathType Leaf) -or (Get-Item -LiteralPath $intermediate).Length -eq 0) {
        throw 'MoonSec frontend did not produce bytecode'
    }
    & $Slatt run $rules $intermediate --emit lua51 -o $OutputFile
    if ($LASTEXITCODE -ne 0) { throw 'Slatt could not rewrite the decoded MoonSec chunk' }
} finally {
    [IO.File]::Delete($intermediate)
    if (-not [IO.Directory]::EnumerateFileSystemEntries($workspace).GetEnumerator().MoveNext()) {
        [IO.Directory]::Delete($workspace)
    }
}
