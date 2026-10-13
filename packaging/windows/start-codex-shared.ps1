param([switch]$CheckOnly)
$ErrorActionPreference = 'Stop'

function Show-LaunchMessage([string]$Message) {
    Add-Type -AssemblyName System.Windows.Forms
    [System.Windows.Forms.MessageBox]::Show($Message, 'PhotoCraft Codex connection', 'OK', 'Information') | Out-Null
}

try {
    $root = Join-Path $env:APPDATA 'Photocraft/CodexBridge'
    $config = Get-Content -Raw -LiteralPath (Join-Path $root 'config.json') | ConvertFrom-Json
    if (-not $config.enabled -or -not (Test-Path -LiteralPath $config.executable -PathType Leaf)) { throw 'PhotoCraft shared connection is not installed.' }
    $package = Get-AppxPackage -Name OpenAI.Codex | Select-Object -First 1
    if (-not $package) { throw 'Codex desktop is not installed for this Windows account.' }
    $desktop = Join-Path $package.InstallLocation 'app/ChatGPT.exe'
    if (-not (Test-Path -LiteralPath $desktop -PathType Leaf)) { throw 'Cannot locate the installed Codex desktop.' }
    if ($CheckOnly) {
        Write-Output 'Shared component and desktop target are valid. Desktop launch mode: Normal. No process was started.'
        return
    }
    if (Get-Process -Name ChatGPT -ErrorAction SilentlyContinue) {
        Show-LaunchMessage 'Codex is already running. Use its taskbar window. To restart, finish its tasks and fully quit Codex first. No additional process was started.'
        return
    }
    $env:CODEX_CLI_PATH = $config.executable
    Remove-Item Env:CODEX_APP_SERVER_WS_URL -ErrorAction SilentlyContinue
    # Only the shortcut's PowerShell helper is hidden; the requested desktop UI must be visible.
    Start-Process -FilePath $desktop -WorkingDirectory (Split-Path -Parent $desktop) -WindowStyle Normal
} catch {
    if ($CheckOnly) { throw }
    Show-LaunchMessage ('Could not open Codex: ' + $_.Exception.Message)
    exit 1
}
