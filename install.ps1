#Requires -Version 5.1
<#
.SYNOPSIS
    Installs Theta.

.DESCRIPTION
    Downloads the release for this platform, verifies its SHA-256, and installs
    the binary. Nothing else is touched.

    One-liner:
        irm https://raw.githubusercontent.com/Bhanu4417/theta/main/install.ps1 | iex

    Note that piping to iex cannot take parameters; use the script form to pass any.

.PARAMETER Version
    Version to install, e.g. 0.1.0. Defaults to the latest release.

.PARAMETER Dir
    Destination directory. Defaults to %LOCALAPPDATA%\Programs\Theta.

.PARAMETER DryRun
    Report what would happen without downloading anything.

.PARAMETER Help
    Show this help.

.EXAMPLE
    .\install.ps1 -Version 0.1.0 -Dir C:\tools
#>
[CmdletBinding()]
param(
    [string]$Version = $env:THETA_VERSION,
    [string]$Dir = $env:THETA_INSTALL_DIR,
    [switch]$DryRun,
    [switch]$Help
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

# GitHub requires TLS 1.2; Windows PowerShell 5.1 does not enable it by default.
try {
    [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
} catch {
    Write-Verbose "Could not set TLS 1.2: $_"
}

$Repo = 'Bhanu4417/theta'
$Bin = 'theta'
$BinName = 'Theta'

function Write-Say  { param([string]$Message) Write-Host $Message }
function Write-Warn { param([string]$Message) Write-Warning $Message }
function Stop-Die   { param([string]$Message) Write-Error "error: $Message"; exit 1 }

if ($Help) {
    @'
Theta installer.

  irm https://raw.githubusercontent.com/Bhanu4417/theta/main/install.ps1 | iex

Downloads the release for this platform, verifies its SHA-256, and installs
the binary. Nothing else is touched.

  -Version <v>   install a specific version, e.g. 0.1.0 (default: latest)
  -Dir <path>    install somewhere else (default: %LOCALAPPDATA%\Programs\Theta)
  -DryRun        report what would happen, downloading nothing
  -Help          this text

Environment equivalents: THETA_VERSION, THETA_INSTALL_DIR.
When piped to iex, parameters cannot be passed; use the script form for those.
'@ | Write-Host
    exit 0
}

if (-not $Dir) {
    $base = if ($env:LOCALAPPDATA) { $env:LOCALAPPDATA } else { $env:USERPROFILE }
    if (-not $base) {
        Stop-Die 'cannot determine an install directory; pass -Dir'
    }
    $Dir = Join-Path $base 'Programs\Theta'
}

# --- platform ---------------------------------------------------------------

$env_arch = $env:PROCESSOR_ARCHITECTURE
switch ($env_arch) {
    'AMD64' { $target = 'x86_64-pc-windows-msvc' }
    'x86'   { $target = 'x86_64-pc-windows-msvc' }  # 32-bit Windows runs the x64 build under WOW64
    'ARM64' {
        # No native ARM64 build is published; Windows emulates x64.
        Write-Warn "No native ARM64 build; installing the x64 build (runs under emulation)."
        $target = 'x86_64-pc-windows-msvc'
    }
    default { Stop-Die "unsupported architecture: $env_arch" }
}

$asset = "$Bin-$target.zip"

# --- version ----------------------------------------------------------------

if (-not $Version) {
    Write-Say 'Resolving the latest version...'
    try {
        $release = Invoke-RestMethod -Uri "https://api.github.com/repos/$Repo/releases/latest" `
            -Headers @{ 'User-Agent' = 'theta-installer' }
        $Version = $release.tag_name
    } catch {
        Stop-Die "could not determine the latest version ($_). Set -Version."
    }
}
$Version = $Version -replace '^v', ''

$base = if ($env:THETA_BASE_URL) { $env:THETA_BASE_URL } else { "https://github.com/$Repo/releases/download" }
$url = "$base/v$Version/$asset"
$dest = Join-Path $Dir "$Bin.exe"

Write-Say "$BinName $Version"
Write-Say "  platform: windows/$env_arch ($target)"
Write-Say "  from:     $url"
Write-Say "  into:     $dest"

if ($DryRun) {
    Write-Say 'dry run: nothing downloaded'
    exit 0
}

# --- download and verify ----------------------------------------------------

$tmp = Join-Path ([IO.Path]::GetTempPath()) ("theta-" + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Force -Path $tmp | Out-Null

try {
    Write-Say 'Downloading...'
    $zip = Join-Path $tmp $asset
    try {
        Invoke-WebRequest -Uri $url -OutFile $zip -UseBasicParsing
    } catch {
        Stop-Die "download failed: $url`n$_"
    }

    Write-Say 'Verifying checksum...'
    $expected = $null
    try {
        $sumRaw = (Invoke-WebRequest -Uri "$url.sha256" -UseBasicParsing).Content
        # Content comes back as byte[] for some responses and PS versions.
        # Splitting that yields ASCII codes instead of the digest, so decode it
        # to text first.
        if ($sumRaw -is [byte[]]) {
            $sumRaw = [System.Text.Encoding]::UTF8.GetString($sumRaw)
        }
        $candidate = (($sumRaw -split '\s+') | Where-Object { $_ } | Select-Object -First 1)
        if ($candidate -match '^[0-9a-fA-F]{64}$') {
            $expected = $candidate.ToLower()
        } else {
            Write-Warn "published checksum is not a SHA-256 digest; skipping verification"
        }
    } catch {
        Write-Warn 'no checksum published for this release; skipping verification'
    }

    if ($expected) {
        $actual = (Get-FileHash -Path $zip -Algorithm SHA256).Hash.ToLower()
        if ($expected -ne $actual) {
            Stop-Die "checksum mismatch`n  expected $expected`n  got      $actual`nDo not use this download."
        }
        Write-Say '  ok'
    }

    Write-Say 'Extracting...'
    Expand-Archive -Path $zip -DestinationPath $tmp -Force
    $exe = Join-Path $tmp "$Bin.exe"
    if (-not (Test-Path $exe)) { Stop-Die "the archive did not contain $Bin.exe" }

    New-Item -ItemType Directory -Force -Path $Dir | Out-Null

    # Stage beside the target, then move: replacing a running process's file
    # directly can fail, and a rename is atomic.
    $staged = Join-Path $Dir ".$Bin.new.$PID.exe"
    Copy-Item -Path $exe -Destination $staged -Force
    Move-Item -Path $staged -Destination $dest -Force

    Write-Say "Installed $dest"
} finally {
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}

# --- PATH -------------------------------------------------------------------

$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if (-not $userPath) { $userPath = '' }
$entries = $userPath -split ';' | Where-Object { $_ -ne '' }
if ($entries -notcontains $Dir) {
    $newPath = (@($entries) + $Dir) -join ';'
    [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
    Write-Say ''
    Write-Say "Added $Dir to your user PATH."
    Write-Say 'Open a new terminal for it to take effect.'
}

Write-Say ''
Write-Say "Run '$Bin --help' to get started, then '$Bin' to open a workspace."
