# Monocle CLI installer for Windows (PowerShell).
# Downloads a prebuilt standalone binary from GitHub Releases — no Node, no npm.
#   irm https://raw.githubusercontent.com/warmblood-kr/monocle-cli/main/install.ps1 | iex
#   $env:MONOCLE_VERSION="v0.5.0"; irm .../install.ps1 | iex   # pin a version

$ErrorActionPreference = "Stop"

$Repo = "warmblood-kr/monocle-cli"
$Binary = "monocle"

function Get-LatestVersion {
    $rel = Invoke-RestMethod "https://api.github.com/repos/$Repo/releases/latest"
    return $rel.tag_name
}

$Version = if ($env:MONOCLE_VERSION) { $env:MONOCLE_VERSION } else { Get-LatestVersion }
if (-not $Version) { throw "Could not determine latest version" }

$Platform = "windows-x64"
$InstallDir = if ($env:MONOCLE_INSTALL_DIR) { $env:MONOCLE_INSTALL_DIR } else { "$env:LOCALAPPDATA\Programs\monocle" }

Write-Host "Installing monocle $Version for $Platform..."
Write-Host "Target: $InstallDir"

New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null

$Asset = "monocle-$Platform.zip"
$Url = "https://github.com/$Repo/releases/download/$Version/$Asset"
$Tmp = New-Item -ItemType Directory -Force -Path (Join-Path $env:TEMP ([System.Guid]::NewGuid().ToString()))
$Zip = Join-Path $Tmp $Asset

Invoke-WebRequest -Uri $Url -OutFile $Zip
Expand-Archive -Path $Zip -DestinationPath $Tmp -Force
Move-Item -Force (Join-Path $Tmp "$Binary.exe") (Join-Path $InstallDir "$Binary.exe")
Remove-Item -Recurse -Force $Tmp

Write-Host "Installed: $InstallDir\$Binary.exe"

$userPath = [Environment]::GetEnvironmentVariable("Path", "User")
if ($userPath -notlike "*$InstallDir*") {
    $newUserPath = if ($userPath) { "$InstallDir;$userPath" } else { $InstallDir }
    [Environment]::SetEnvironmentVariable("Path", $newUserPath, "User")
    Write-Host ""
    Write-Host "Added $InstallDir to your PATH."
}

# SetEnvironmentVariable only persists to the registry for *future* processes —
# this already-running shell keeps its own PATH snapshot, so `monocle` below
# would still fail to resolve without also updating this process's $env:Path.
if ($env:Path -notlike "*$InstallDir*") {
    $env:Path = "$InstallDir;$env:Path"
}

Write-Host ""
Write-Host "Run 'monocle login' to get started."
