$ErrorActionPreference = 'Stop'
$root = Join-Path $env:APPDATA 'Photocraft/CodexBridge'
$backup = Get-Content -Raw -LiteralPath (Join-Path $root 'installation.json') | ConvertFrom-Json
$current = [Environment]::GetEnvironmentVariable('CODEX_CLI_PATH','User')
if ($current -ne $backup.installedExecutable) { throw 'CODEX_CLI_PATH was changed outside PhotoCraft; it has not been overwritten.' }
[Environment]::SetEnvironmentVariable('CODEX_CLI_PATH',$backup.previousCliPath,'User')
$path = Join-Path $root 'config.json'
$config = Get-Content -Raw -LiteralPath $path | ConvertFrom-Json
$config.enabled = $false
[IO.File]::WriteAllText($path,($config | ConvertTo-Json),[Text.UTF8Encoding]::new($false))
Write-Output 'Connection disabled for the next launch. No applications were closed and no conversation history was changed.'
