<#
.SYNOPSIS  Builds the FerroSurf Windows installer with Inno Setup.
.EXAMPLE   .\packaging\windows\build-installer.ps1 -Version 0.1.0 [-SkipBuild]
#>
param(
  [string]$Version = "",
  [switch]$SkipBuild
)
$ErrorActionPreference = "Stop"
$Root = Resolve-Path (Join-Path $PSScriptRoot "..\..")
Set-Location $Root

if (-not $Version) {
  $m = Select-String -Path "Cargo.toml" -Pattern '^\s*version\s*=\s*"([^"]+)"' | Select-Object -First 1
  $Version = if ($m) { $m.Matches[0].Groups[1].Value } else { "0.1.0" }
}

if (-not $SkipBuild) {
  cargo build --release -p ferrosurf
  if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
}
if (-not (Test-Path "target\release\ferrosurf.exe")) { throw "target\release\ferrosurf.exe not found" }

$iscc = (Get-Command iscc.exe -ErrorAction SilentlyContinue).Source
if (-not $iscc) {
  foreach ($p in @("${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe", "$env:ProgramFiles\Inno Setup 6\ISCC.exe")) {
    if (Test-Path $p) { $iscc = $p; break }
  }
}
if (-not $iscc) { throw "Inno Setup (iscc.exe) not found. Install: choco install innosetup" }

New-Item -ItemType Directory -Force dist | Out-Null
& $iscc "/DAppVersion=$Version" "packaging\windows\ferrosurf.iss"
if ($LASTEXITCODE -ne 0) { throw "iscc failed" }
Write-Host "Installer written to dist\FerroSurf-$Version-setup.exe"
