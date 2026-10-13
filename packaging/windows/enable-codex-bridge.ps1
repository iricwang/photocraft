param(
    [string]$PhotoCraft = (Join-Path $PSScriptRoot 'photocraft.exe'),
    [string]$Bridge = (Join-Path $PSScriptRoot 'photocraft-codex-bridge.exe'),
    [string]$Codex
)
$ErrorActionPreference = 'Stop'
if (-not $Codex) {
    $localBin = Join-Path $env:LOCALAPPDATA 'OpenAI/Codex/bin'
    $candidate = Get-ChildItem -LiteralPath $localBin -Filter codex.exe -File -Recurse -ErrorAction SilentlyContinue |
        Sort-Object LastWriteTimeUtc -Descending | Select-Object -First 1
    if ($candidate) { $Codex = $candidate.FullName }
    else { $Codex = (Get-Command codex.exe -ErrorAction Stop).Source }
}
$PhotoCraft = (Resolve-Path -LiteralPath $PhotoCraft).Path
$Bridge = (Resolve-Path -LiteralPath $Bridge).Path
$Codex = (Resolve-Path -LiteralPath $Codex).Path
if ($Bridge -eq $Codex) { throw 'The real Codex executable cannot be the bridge.' }
$root = Join-Path $env:APPDATA 'Photocraft/CodexBridge'
$null = New-Item -ItemType Directory -Force -Path $root
$identity = [System.Security.Principal.WindowsIdentity]::GetCurrent().Name
& icacls.exe $root /inheritance:r /grant:r "${identity}:(OI)(CI)F" | Out-Null
if ($LASTEXITCODE -ne 0) { throw 'Cannot protect local connection credentials.' }
$configPath = Join-Path $root 'config.json'
$backupPath = Join-Path $root 'installation.json'
$previous = [Environment]::GetEnvironmentVariable('CODEX_CLI_PATH','User')
if (Test-Path -LiteralPath $backupPath) {
    $backup = Get-Content -Raw -LiteralPath $backupPath | ConvertFrom-Json
    if ($previous -and $previous -ne $backup.installedExecutable) {
        throw 'CODEX_CLI_PATH was changed outside PhotoCraft; review it before installing.'
    }
} else {
    $backup = [pscustomobject]@{version=1; previousCliPath=$previous; installedExecutable=$null}
}
$version = (Get-FileHash -LiteralPath $Bridge -Algorithm SHA256).Hash.Substring(0,16).ToLowerInvariant()
$componentDir = Join-Path $root "components/$version"
$null = New-Item -ItemType Directory -Force -Path $componentDir
$installed = Join-Path $componentDir 'photocraft-codex-bridge.exe'
if (-not (Test-Path -LiteralPath $installed)) { Copy-Item -LiteralPath $Bridge -Destination $installed }
foreach ($name in @('start-codex-shared.ps1','disable-codex-bridge.ps1')) {
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot $name) -Destination (Join-Path $root $name) -Force
}
$config = @{version=1;enabled=$true;realCodex=$Codex;executable=$installed}
$oldConfig = if (Test-Path -LiteralPath $configPath) { [IO.File]::ReadAllText($configPath) } else { $null }
[IO.File]::WriteAllText($configPath, ($config | ConvertTo-Json), [Text.UTF8Encoding]::new($false))
try {
    & $PhotoCraft --prepare-codex-bridge
    if ($LASTEXITCODE -ne 0) { throw 'Existing conversation registration failed.' }
    $backup.installedExecutable = $installed
    [IO.File]::WriteAllText($backupPath, ($backup | ConvertTo-Json), [Text.UTF8Encoding]::new($false))
    [Environment]::SetEnvironmentVariable('CODEX_CLI_PATH',$installed,'User')
} catch {
    if ($null -ne $oldConfig) { [IO.File]::WriteAllText($configPath,$oldConfig,[Text.UTF8Encoding]::new($false)) }
    else { $config.enabled=$false; [IO.File]::WriteAllText($configPath,($config | ConvertTo-Json),[Text.UTF8Encoding]::new($false)) }
    throw
}
$shortcutPath = Join-Path ([Environment]::GetFolderPath('Desktop')) 'Codex - PhotoCraft Shared.lnk'
if (Test-Path -LiteralPath $shortcutPath) {
    $existing = (New-Object -ComObject WScript.Shell).CreateShortcut($shortcutPath)
    if (-not $existing.Arguments.Contains((Join-Path $root 'start-codex-shared.ps1'))) { throw 'An unrelated shortcut already uses the requested name.' }
}
$shortcut = (New-Object -ComObject WScript.Shell).CreateShortcut($shortcutPath)
$shortcut.TargetPath = (Get-Command powershell.exe).Source
$shortcut.Arguments = '-NoProfile -WindowStyle Hidden -ExecutionPolicy Bypass -File "' + (Join-Path $root 'start-codex-shared.ps1') + '"'
$shortcut.WindowStyle = 7
$shortcut.Description = 'Start Codex with the built-in PhotoCraft shared connection'
$shortcut.Save()
Write-Output 'Installed. No running app was closed. After all Codex tasks finish, quit Codex and use the Codex - PhotoCraft Shared desktop shortcut.'
