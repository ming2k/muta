# install.ps1 — verified per-user installer for muta on Windows.
#
# Usage:
#   irm https://raw.githubusercontent.com/ming2k/muta/main/install.ps1 | iex
# Optional environment overrides:
#   MUTA_VERSION=0.30.2
#   MUTA_INSTALL_DIR=C:\Tools\muta

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repo = 'ming2k/muta'
$target = 'x86_64-pc-windows-msvc'
$version = $env:MUTA_VERSION

if ([System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture -ne 'X64') {
    throw 'This release provides Windows x86-64 only.'
}

if ([string]::IsNullOrWhiteSpace($version)) {
    Write-Host '› Looking up the latest release...'
    $release = Invoke-RestMethod -Uri "https://api.github.com/repos/$repo/releases/latest"
    $version = $release.tag_name
}
$version = $version -replace '^v', ''

$installDir = $env:MUTA_INSTALL_DIR
if ([string]::IsNullOrWhiteSpace($installDir)) {
    $installDir = Join-Path $env:LOCALAPPDATA 'Programs\muta\bin'
}

$archive = "muta-$version-$target.zip"
$baseUrl = "https://github.com/$repo/releases/download/v$version"
$temporary = Join-Path ([System.IO.Path]::GetTempPath()) ("muta-install-" + [guid]::NewGuid())
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
    $binaryNames = @('muta', 'mutx')
    $sources = @{}
    foreach ($binaryName in $binaryNames) {
        $source = Get-ChildItem -Path $temporary -Filter "$binaryName.exe" -File -Recurse |
            Select-Object -First 1
        if ($null -eq $source) {
            throw "$binaryName.exe was not found in the release archive."
        }
        $sources[$binaryName] = $source.FullName
    }

    New-Item -ItemType Directory -Force -Path $installDir | Out-Null
    $existingCore = Join-Path $installDir 'muta.exe'
    if (Test-Path -LiteralPath $existingCore) {
        # A running daemon keeps the old core image open on Windows. Ask it to
        # drain before replacing the pair.
        & $existingCore daemon stop 2>$null | Out-Null
    }

    $records = @()
    try {
        foreach ($binaryName in $binaryNames) {
            $destination = Join-Path $installDir "$binaryName.exe"
            $record = [pscustomobject]@{
                Destination = $destination
                Staged = Join-Path $installDir ("$binaryName.exe.new-" + [guid]::NewGuid())
                Backup = Join-Path $installDir ("$binaryName.exe.backup-" + [guid]::NewGuid())
                HadExisting = Test-Path -LiteralPath $destination
            }
            $records += $record
            Copy-Item -LiteralPath $sources[$binaryName] -Destination $record.Staged

            if ($record.HadExisting) {
                [System.IO.File]::Replace(
                    $record.Staged,
                    $record.Destination,
                    $record.Backup,
                    $true
                )
            }
            else {
                Move-Item -LiteralPath $record.Staged -Destination $record.Destination
            }
        }

        foreach ($record in $records) {
            $installedVersion = (& $record.Destination --version | Out-String).Trim()
            if ($LASTEXITCODE -ne 0 -or $installedVersion -notmatch [regex]::Escape($version)) {
                throw "Installed binary failed version validation (expected v$version, got '$installedVersion')."
            }
        }
        foreach ($record in $records) {
            if (Test-Path -LiteralPath $record.Backup) {
                Remove-Item -LiteralPath $record.Backup -Force
            }
        }
    }
    catch {
        for ($i = $records.Count - 1; $i -ge 0; $i--) {
            $record = $records[$i]
            if (Test-Path -LiteralPath $record.Backup) {
                [System.IO.File]::Replace(
                    $record.Backup,
                    $record.Destination,
                    $null,
                    $true
                )
            }
            elseif (-not $record.HadExisting -and (Test-Path -LiteralPath $record.Destination)) {
                Remove-Item -LiteralPath $record.Destination -Force
            }
            if (Test-Path -LiteralPath $record.Staged) {
                Remove-Item -LiteralPath $record.Staged -Force
            }
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

    Write-Host "✓ Installed muta and mutx v$version to $installDir"
    Write-Host '  Run mutx to start.'
}
finally {
    if (Test-Path -LiteralPath $temporary) {
        Remove-Item -LiteralPath $temporary -Recurse -Force
    }
}
