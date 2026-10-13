$ErrorActionPreference = 'Stop'
$tokens = $null
$errors = $null
$ast = [System.Management.Automation.Language.Parser]::ParseFile((Join-Path $PSScriptRoot 'start-codex-shared.ps1'),[ref]$tokens,[ref]$errors)
if ($errors.Count) { throw ($errors | Out-String) }
$starts = @($ast.FindAll({ param($node) $node -is [System.Management.Automation.Language.CommandAst] -and $node.GetCommandName() -eq 'Start-Process' },$true))
if ($starts.Count -ne 1) { throw 'Expected one explicit desktop launch site.' }
$elements = $starts[0].CommandElements
$windowStyle = $null
$workingDirectory = $false
for ($i = 0; $i -lt $elements.Count; $i++) {
    if ($elements[$i] -is [System.Management.Automation.Language.CommandParameterAst]) {
        if ($elements[$i].ParameterName -eq 'WindowStyle' -and $i + 1 -lt $elements.Count) { $windowStyle = $elements[$i + 1].Extent.Text }
        if ($elements[$i].ParameterName -eq 'WorkingDirectory') { $workingDirectory = $true }
    }
}
if ($windowStyle -ne 'Normal' -or -not $workingDirectory) { throw 'Desktop must launch visibly with its application working directory.' }
$source = $ast.Extent.Text
if (-not $source.Contains('if ($CheckOnly)') -or -not $source.Contains('MessageBox]::Show')) { throw 'Launcher must support nonlaunching validation and visible failure reporting.' }
Write-Output 'Launcher syntax, visible desktop mode, working directory, validation and error reporting: passed.'
