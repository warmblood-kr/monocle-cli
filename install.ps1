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

$SumsUrl = "https://github.com/$Repo/releases/download/$Version/SHA256SUMS"
$SumsFile = Join-Path $Tmp "SHA256SUMS"
$sumsAvailable = $true
try {
    Invoke-WebRequest -Uri $SumsUrl -OutFile $SumsFile -ErrorAction Stop
} catch {
    Write-Host "Warning: SHA256SUMS not available, skipping checksum verification."
    $sumsAvailable = $false
}

if ($sumsAvailable) {
    $line = Select-String -Path $SumsFile -Pattern ([regex]::Escape($Asset)) -SimpleMatch | Select-Object -First 1
    if ($line) {
        $expected = ($line.Line -split '\s+')[0].ToLower()
        $actual = (Get-FileHash -Path $Zip -Algorithm SHA256).Hash.ToLower()
        if ($expected -ne $actual) {
            throw "Checksum verification failed for $Asset (expected $expected, got $actual)"
        }
        Write-Host "Checksum verified: $Asset"
    } else {
        Write-Host "Warning: no checksum entry for $Asset, skipping checksum verification."
    }
}

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
