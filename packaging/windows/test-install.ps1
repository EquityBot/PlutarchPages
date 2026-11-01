<#
.SYNOPSIS
  Install an MSI for the current account, run it, uninstall it and check nothing is left. With
  -PreviousPath, then install that older MSI, upgrade it to this one, check the result matches the
  fresh install, run it, uninstall it and check nothing is left. Changes the machine: for disposable
  CI runners.
.EXAMPLE
  pwsh packaging/windows/test-install.ps1 -Path dist/release/pdfcraft-0.16.1-windows-x64.msi -ExpectedVersion 0.16.1 -PreviousPath pdfcraft-0.15.0-windows-x64.msi -PreviousVersion 0.15.0
#>
param(
  [Parameter(Mandatory)] [string] $Path,
  [Parameter(Mandatory)] [string] $ExpectedVersion,
  [string] $PreviousPath,
  [string] $PreviousVersion
)
$ErrorActionPreference = 'Stop'
if ([bool] $PreviousPath -ne [bool] $PreviousVersion) { throw '-PreviousPath and -PreviousVersion go together' }
# MajorUpgrade allows same-version upgrades, so this would pass without showing a version change.
if ($PreviousVersion -eq $ExpectedVersion) { throw "Upgrading from $PreviousVersion to itself tests nothing" }
$msi = (Resolve-Path -LiteralPath $Path).Path
$key = 'HKCU:\Software\PlutarchPages'
$folder = Join-Path $env:LOCALAPPDATA 'Programs\PlutarchPages'
$programs = [Environment]::GetFolderPath('Programs')
$desktop = [Environment]::GetFolderPath('Desktop')
$owned = @(
  $folder,
  $key,
  'HKCU:\Software\Classes\PlutarchPages.Document',
  'HKCU:\Software\Classes\SystemFileAssociations\.pdf\shell\PlutarchPages.Edit',
  'HKCU:\Software\Classes\SystemFileAssociations\.png\shell\PlutarchPages.CreatePdf',
  (Join-Path $programs 'PlutarchPages.lnk')
)

$installer = New-Object -ComObject WindowsInstaller.Installer
$database = $installer.OpenDatabase($msi, 0)
$view = $database.OpenView('SELECT `Value` FROM `Property` WHERE `Property` = ''UpgradeCode''')
[void] $view.Execute()
$upgradeCode = $view.Fetch().StringData(1)
[void] $view.Close()
# Don't hold the file open while msiexec installs from it.
[void] [Runtime.InteropServices.Marshal]::FinalReleaseComObject($view)
[void] [Runtime.InteropServices.Marshal]::FinalReleaseComObject($database)

function Invoke-Msi([string[]] $Arguments) {
  $process = Start-Process msiexec.exe -ArgumentList $Arguments -Wait -PassThru
  if ($process.ExitCode -ne 0) { throw "msiexec $($Arguments -join ' ') exited with $($process.ExitCode)" }
}

# The installed product with this upgrade code. Two would mean an upgrade left the old one behind.
function Get-OnlyProduct {
  $products = @($installer.RelatedProducts($upgradeCode))
  if ($products.Count -ne 1) { throw "Expected 1 installed product, found $($products.Count): $($products -join ', ')" }
  return $products[0]
}

function Get-RegistryLine([string] $Pattern) {
  foreach ($root in @(Get-Item -Path $Pattern -ErrorAction SilentlyContinue)) {
    foreach ($registryKey in @($root) + @(Get-ChildItem -LiteralPath $root.PSPath -Recurse)) {
      $registryKey.Name
      foreach ($name in $registryKey.GetValueNames()) { "$($registryKey.Name)|$name|$($registryKey.GetValue($name))" }
    }
  }
}

# Everything an installed copy adds, as lines to compare: each file with its hash, shortcuts, the
# registration in pdfcraft.wxs and the Add/Remove Programs entries.
function Get-InstallState {
  if (Test-Path -LiteralPath $folder) {
    Get-ChildItem -LiteralPath $folder -Recurse -File -Force | ForEach-Object { "$($_.FullName)|$((Get-FileHash -LiteralPath $_.FullName).Hash)" }
  }
  Get-ChildItem -LiteralPath $programs, $desktop -Filter 'PlutarchPages*' -Force | ForEach-Object FullName
  @(
    $key,
    'HKCU:\Software\Classes\PlutarchPages.Document',
    'HKCU:\Software\Classes\.pdf\OpenWithProgids',
    'HKCU:\Software\Classes\Applications\pdfcraft.exe',
    'HKCU:\Software\Classes\SystemFileAssociations\*\shell\PlutarchPages.*',
    'HKCU:\Software\Microsoft\Windows\CurrentVersion\App Paths\pdfcraft.exe',
    'HKCU:\Software\RegisteredApplications'
  ) | ForEach-Object { Get-RegistryLine $_ } | Where-Object { $_ -match 'PlutarchPages|pdfcraft' }
  Get-ItemProperty -Path @(
    'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\*',
    'HKLM:\Software\Microsoft\Windows\CurrentVersion\Uninstall\*',
    'HKLM:\Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\*'
  ) -ErrorAction SilentlyContinue | Where-Object { $_.DisplayName -eq 'PlutarchPages' } | ForEach-Object { "Add/Remove Programs|$($_.PSChildName)|$($_.DisplayVersion)" }
}

function Assert-CliVersion([string] $Version) {
  $cli = Join-Path $folder 'pdfcraft-cli.exe'
  $reported = (& $cli --version) -join "`n"
  if ($LASTEXITCODE -ne 0) { throw "pdfcraft-cli --version exited with $LASTEXITCODE" }
  if ($reported -notmatch [regex]::Escape($Version)) { throw "pdfcraft-cli reported '$reported', expected $Version" }
}

function Assert-Installed {
  foreach ($path in $owned) {
    if (-not (Test-Path -LiteralPath $path)) { throw "The install didn't create $path" }
  }
  $installed = (Get-ItemProperty -LiteralPath $key -Name InstallDir).InstallDir.TrimEnd('\')
  if ($installed -ne $folder) { throw "InstallDir is '$installed', expected '$folder'" }
}

# Starts the app, waits for its window, stops it and returns the window's title.
function Test-App {
  $app = Start-Process (Join-Path $folder 'pdfcraft.exe') -PassThru
  $deadline = (Get-Date).AddSeconds(60)
  do {
    Start-Sleep -Seconds 2
    $app.Refresh()
    if ($app.HasExited) { throw "pdfcraft.exe exited with $($app.ExitCode) before showing a window" }
  } until ($app.MainWindowHandle -ne 0 -or (Get-Date) -gt $deadline)
  if ($app.MainWindowHandle -eq 0) { throw 'pdfcraft.exe showed no window within 60 seconds' }
  $title = $app.MainWindowTitle
  Stop-Process -Id $app.Id -Force
  $app.WaitForExit()
  return $title
}

function Uninstall-AndCheck {
  Invoke-Msi @('/x', "`"$msi`"", '/qn', '/norestart')
  foreach ($path in $owned) {
    if (Test-Path -LiteralPath $path) { throw "$path survived the uninstall" }
  }
  $left = @(Get-InstallState)
  if ($left.Count -ne 0) { throw "The uninstall left:`n$($left -join "`n")" }
  $products = @($installer.RelatedProducts($upgradeCode))
  if ($products.Count -ne 0) { throw "The uninstall left products: $($products -join ', ')" }
}

Write-Output 'Installing'
Invoke-Msi @('/i', "`"$msi`"", '/qn', '/norestart')
Assert-Installed
$freshProduct = Get-OnlyProduct
$fresh = @(Get-InstallState)

Write-Output 'Checking the CLI version'
Assert-CliVersion $ExpectedVersion

Write-Output 'Starting and stopping the app'
$title = Test-App

Write-Output 'Uninstalling'
Uninstall-AndCheck
Write-Output "PASS installed, ran $ExpectedVersion, showed the window '$title' and uninstalled."

if (-not $PreviousPath) { return }

# You only install twice: this second pass is the one a user who already has the app gets.
$previous = (Resolve-Path -LiteralPath $PreviousPath).Path
Write-Output "Installing $PreviousVersion"
Invoke-Msi @('/i', "`"$previous`"", '/qn', '/norestart')
Assert-Installed
Assert-CliVersion $PreviousVersion
$oldProduct = Get-OnlyProduct
$oldVersion = $installer.ProductInfo($oldProduct, 'VersionString')

Write-Output "Upgrading to $ExpectedVersion"
Invoke-Msi @('/i', "`"$msi`"", '/qn', '/norestart')
Assert-Installed
Assert-CliVersion $ExpectedVersion
$newProduct = Get-OnlyProduct
$newVersion = $installer.ProductInfo($newProduct, 'VersionString')
if ($newProduct -ne $freshProduct) { throw "Installed product is $newProduct, expected $freshProduct" }
if ($newVersion -eq $oldVersion) { throw "The installed version is still $oldVersion" }

Write-Output 'Comparing with the fresh install'
$difference = @(Compare-Object $fresh @(Get-InstallState))
if ($difference.Count -ne 0) {
  $lines = $difference | ForEach-Object { "$($_.SideIndicator) $($_.InputObject)" }
  throw "The upgrade differs from a fresh install (=> only after the upgrade):`n$($lines -join "`n")"
}

Write-Output 'Starting and stopping the upgraded app'
$title = Test-App

Write-Output 'Uninstalling the upgrade'
Uninstall-AndCheck
Write-Output "PASS upgraded $oldVersion to $newVersion, ran it, showed the window '$title' and uninstalled."
