<#
.SYNOPSIS
  Install an MSI for the current account, run it, uninstall it and check nothing is left. Changes the
  machine: for disposable CI runners.
.EXAMPLE
  pwsh packaging/windows/test-install.ps1 -Path dist/release/pdfcraft-0.15.0-windows-x64.msi -ExpectedVersion 0.15.0
#>
param(
  [Parameter(Mandatory)] [string] $Path,
  [Parameter(Mandatory)] [string] $ExpectedVersion
)
$ErrorActionPreference = 'Stop'
$msi = (Resolve-Path -LiteralPath $Path).Path
$key = 'HKCU:\Software\PlutarchPages'
$folder = Join-Path $env:LOCALAPPDATA 'Programs\PlutarchPages'

function Invoke-Msi([string[]] $Arguments) {
  $process = Start-Process msiexec.exe -ArgumentList $Arguments -Wait -PassThru
  if ($process.ExitCode -ne 0) { throw "msiexec $($Arguments -join ' ') exited with $($process.ExitCode)" }
}

Invoke-Msi @('/i', "`"$msi`"", '/qn', '/norestart')
$installed = (Get-ItemProperty -LiteralPath $key -Name InstallDir).InstallDir.TrimEnd('\')
if ($installed -ne $folder) { throw "InstallDir is '$installed', expected '$folder'" }

$cli = Join-Path $folder 'pdfcraft-cli.exe'
$version = (& $cli --version) -join "`n"
if ($LASTEXITCODE -ne 0) { throw "pdfcraft-cli --version exited with $LASTEXITCODE" }
if ($version -notmatch [regex]::Escape($ExpectedVersion)) { throw "pdfcraft-cli reported '$version'" }

$app = Start-Process (Join-Path $folder 'pdfcraft.exe') -PassThru
Start-Sleep -Seconds 15
if ($app.HasExited) { throw "pdfcraft.exe exited with $($app.ExitCode) within 15 seconds" }
Stop-Process -Id $app.Id -Force
$app.WaitForExit()

Invoke-Msi @('/x', "`"$msi`"", '/qn', '/norestart')
if (Test-Path -LiteralPath (Join-Path $folder 'pdfcraft.exe')) { throw 'pdfcraft.exe survived the uninstall.' }
if ($null -ne (Get-ItemProperty -LiteralPath $key -Name InstallDir -ErrorAction SilentlyContinue)) {
  throw 'The InstallDir record survived the uninstall.'
}
Write-Output "PASS installed, ran $ExpectedVersion and uninstalled."
