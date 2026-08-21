# install.ps1 — verified per-user installer for neenee on Windows.
#
# Usage:
#   irm https://raw.githubusercontent.com/ming2k/neenee/main/install.ps1 | iex
# Optional environment overrides:
#   NEENEE_VERSION=0.30.2
#   NEENEE_INSTALL_DIR=C:\Tools\neenee

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repo = 'ming2k/neenee'
$target = 'x86_64-pc-windows-msvc'
$version = $env:NEENEE_VERSION

if ([System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture -ne 'X64') {
    throw 'This release provides Windows x86-64 only.'
}

if ([string]::IsNullOrWhiteSpace($version)) {
    Write-Host '› Looking up the latest release...'
    $release = Invoke-RestMethod -Uri "https://api.github.com/repos/$repo/releases/latest"
    $version = $release.tag_name
}
$version = $version -replace '^v', ''

$installDir = $env:NEENEE_INSTALL_DIR
if ([string]::IsNullOrWhiteSpace($installDir)) {
    $installDir = Join-Path $env:LOCALAPPDATA 'Programs\neenee\bin'
}

$archive = "neenee-$version-$target.zip"
$baseUrl = "https://github.com/$repo/releases/download/v$version"
$temporary = Join-Path ([System.IO.Path]::GetTempPath()) ("neenee-install-" + [guid]::NewGuid())
New-Item -ItemType Directory -Path $temporary | Out-Null

try {
    $archivePath = Join-Path $temporary $archive
    $checksumPath = "$archivePath.sha256"
    Write-Host "› Downloading $archive"
    Invoke-WebRequest -UseBasicParsing -Uri "$baseUrl/$archive" -OutFile $archivePath
    Invoke-WebRequest -UseBasicParsing -Uri "$baseUrl/$archive.sha256" -OutFile $checksumPath

    $expected = ((Get-Content -Raw $checksumPath).Trim() -split '\s+')[0].ToLowerInvariant()
    $actual = (Get-FileHash -Algorithm SHA256 $archivePath).Hash.ToLowerInvariant()
    if ($actual -ne $expected) {
        throw 'SHA-256 checksum mismatch; download was not installed.'
    }

    Expand-Archive -LiteralPath $archivePath -DestinationPath $temporary
    $source = Get-ChildItem -Path $temporary -Filter neenee.exe -File -Recurse |
        Select-Object -First 1
    if ($null -eq $source) {
        throw 'neenee.exe was not found in the release archive.'
    }

    New-Item -ItemType Directory -Force -Path $installDir | Out-Null
    $destination = Join-Path $installDir 'neenee.exe'
    $staged = Join-Path $installDir ("neenee.exe.new-" + [guid]::NewGuid())
    $backup = Join-Path $installDir ("neenee.exe.backup-" + [guid]::NewGuid())
    Copy-Item -LiteralPath $source.FullName -Destination $staged
    $hadExistingInstall = Test-Path -LiteralPath $destination

    try {
        if ($hadExistingInstall) {
            # A running daemon keeps the old image open on Windows. Ask it to
            # drain before the atomic replacement; failure remains explicit at
            # File.Replace instead of leaving a partially-updated install.
            & $destination daemon stop 2>$null | Out-Null
            [System.IO.File]::Replace($staged, $destination, $backup, $true)
        }
        else {
            Move-Item -LiteralPath $staged -Destination $destination
        }

        $installedVersion = (& $destination --version | Out-String).Trim()
        if ($LASTEXITCODE -ne 0 -or $installedVersion -notmatch [regex]::Escape($version)) {
            throw "Installed binary failed version validation (expected v$version, got '$installedVersion')."
        }
        if (Test-Path -LiteralPath $backup) {
            Remove-Item -LiteralPath $backup -Force
        }
    }
    catch {
        if (Test-Path -LiteralPath $backup) {
            [System.IO.File]::Replace($backup, $destination, $null, $true)
        }
        elseif (-not $hadExistingInstall -and (Test-Path -LiteralPath $destination)) {
            Remove-Item -LiteralPath $destination -Force
        }
        if (Test-Path -LiteralPath $staged) {
            Remove-Item -LiteralPath $staged -Force
        }
        throw
    }

    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    $entries = @($userPath -split ';' | Where-Object { -not [string]::IsNullOrWhiteSpace($_) })
    if (-not ($entries | Where-Object { $_.TrimEnd('\') -ieq $installDir.TrimEnd('\') })) {
        $newPath = (@($entries) + $installDir) -join ';'
        [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
        $env:Path = "$installDir;$env:Path"
        Write-Host "› Added $installDir to your user PATH (new terminals will inherit it)."
    }

    Write-Host "✓ Installed neenee v$version to $destination"
    Write-Host '  Run neenee to start.'
}
finally {
    if (Test-Path -LiteralPath $temporary) {
        Remove-Item -LiteralPath $temporary -Recurse -Force
    }
}
