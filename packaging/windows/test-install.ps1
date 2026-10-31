<#
.SYNOPSIS
  Install an MSI for the current account, run it, uninstall it and check nothing is left. Changes the
  machine: for disposable CI runners. Upgrades need two releases, so this doesn't cover them.
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
$owned = @(
  $folder,
  $key,
  'HKCU:\Software\Classes\PlutarchPages.Document',
  'HKCU:\Software\Classes\SystemFileAssociations\.pdf\shell\PlutarchPages.Edit',
  'HKCU:\Software\Classes\SystemFileAssociations\.png\shell\PlutarchPages.CreatePdf',
  (Join-Path ([Environment]::GetFolderPath('Programs')) 'PlutarchPages.lnk')
)

function Invoke-Msi([string[]] $Arguments) {
  $process = Start-Process msiexec.exe -ArgumentList $Arguments -Wait -PassThru
  if ($process.ExitCode -ne 0) { throw "msiexec $($Arguments -join ' ') exited with $($process.ExitCode)" }
}

Write-Output 'Installing'
Invoke-Msi @('/i', "`"$msi`"", '/qn', '/norestart')
foreach ($path in $owned) {
  if (-not (Test-Path -LiteralPath $path)) { throw "The install didn't create $path" }
}
$installed = (Get-ItemProperty -LiteralPath $key -Name InstallDir).InstallDir.TrimEnd('\')
if ($installed -ne $folder) { throw "InstallDir is '$installed', expected '$folder'" }

Write-Output 'Checking the CLI version'
$cli = Join-Path $folder 'pdfcraft-cli.exe'
$version = (& $cli --version) -join "`n"
if ($LASTEXITCODE -ne 0) { throw "pdfcraft-cli --version exited with $LASTEXITCODE" }
if ($version -notmatch [regex]::Escape($ExpectedVersion)) { throw "pdfcraft-cli reported '$version'" }

Write-Output 'Starting the app'
$app = Start-Process (Join-Path $folder 'pdfcraft.exe') -PassThru
$deadline = (Get-Date).AddSeconds(60)
do {
  Start-Sleep -Seconds 2
  $app.Refresh()
  if ($app.HasExited) { throw "pdfcraft.exe exited with $($app.ExitCode) before showing a window" }
} until ($app.MainWindowHandle -ne 0 -or (Get-Date) -gt $deadline)
if ($app.MainWindowHandle -eq 0) { throw 'pdfcraft.exe showed no window within 60 seconds' }
$title = $app.MainWindowTitle
Write-Output 'Stopping the app'
Stop-Process -Id $app.Id -Force
$app.WaitForExit()

Write-Output 'Uninstalling'
Invoke-Msi @('/x', "`"$msi`"", '/qn', '/norestart')
foreach ($path in $owned) {
  if (Test-Path -LiteralPath $path) { throw "$path survived the uninstall" }
}
Write-Output "PASS installed, ran $ExpectedVersion, showed the window '$title' and uninstalled."
