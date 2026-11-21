<#
.SYNOPSIS
    Creates a clean, self-contained portable distribution folder for QuantumView.

.PARAMETER App
    Which UI bundle to include: 'all' (default), 'workspace', or 'starter'.

.PARAMETER OutputDir
    Destination directory (defaults to dist\quantumview).

.PARAMETER ExePath
    Path to the release quantumview-servo.exe binary.

.PARAMETER GStreamerDir
    Optional GStreamer MSVC install root containing bin\ and lib\gstreamer-1.0\,
    or a flat directory of GStreamer DLLs.

.PARAMETER AllowIncompleteGStreamer
    Warn and continue when one or more expected GStreamer plugin DLLs are missing.
#>
[CmdletBinding()]
param(
    [ValidateSet('all', 'workspace', 'starter')]
    [string]$App = 'all',
    [string]$OutputDir,
    [string]$ExePath,
    [string]$GStreamerDir,
    [switch]$AllowIncompleteGStreamer
)

$ErrorActionPreference = 'Stop'

$repoRoot = Split-Path -Parent $PSScriptRoot
if ([string]::IsNullOrWhiteSpace($OutputDir)) {
    $folderName = if ($App -eq 'all') { 'quantumview' } else { "quantumview-$App" }
    $OutputDir = Join-Path $repoRoot ("dist\$folderName")
}
if ([string]::IsNullOrWhiteSpace($ExePath)) {
    $ExePath = Join-Path $repoRoot 'probes\servo\target\release\quantumview-servo.exe'
}

if (-not (Test-Path -LiteralPath $ExePath -PathType Leaf)) {
    throw "Release executable not found: $ExePath"
}

$uiSourceDir = Join-Path $repoRoot 'probes\servo\ui'
if (-not (Test-Path -LiteralPath $uiSourceDir -PathType Container)) {
    throw "UI directory not found: $uiSourceDir"
}

$uiPatterns = switch ($App) {
    'all'       { @('*.html', '*.css', '*.js') }
    'workspace' { @('workspace.html', 'workspace.css', 'workspace.js') }
    'starter'   { @('starter.html', 'starter.css', 'starter.js') }
}

$runtimeDlls = @()
$pluginFiles = @()
if (-not [string]::IsNullOrWhiteSpace($GStreamerDir)) {
    if (-not (Test-Path -LiteralPath $GStreamerDir -PathType Container)) {
        throw "GStreamer directory not found: $GStreamerDir"
    }

    $gstRoot = (Resolve-Path -LiteralPath $GStreamerDir).Path
    $gstBin = Join-Path $gstRoot 'bin'
    foreach ($runtimeDir in @($gstBin, $gstRoot)) {
        if (-not (Test-Path -LiteralPath $runtimeDir -PathType Container)) {
            continue
        }
        $dlls = @(Get-ChildItem -LiteralPath $runtimeDir -Filter '*.dll' -File)
        if ($dlls.Name -match '^(?:lib)?gstreamer-1\.0.*\.dll$') {
            $runtimeDlls = $dlls
            break
        }
    }
    if ($runtimeDlls.Count -eq 0) {
        throw "No GStreamer core runtime DLL found under $gstRoot (expected gstreamer-1.0*.dll or libgstreamer-1.0*.dll)"
    }

    # Servo 0.8.0 has 35 common plugins plus gstwasapi on Windows: 36 DLLs total.
    $requiredPlugins = @(
        'gstcoreelements.dll', 'gstnice.dll', 'gstapp.dll', 'gstaudioconvert.dll',
        'gstaudioresample.dll', 'gstgio.dll', 'gstogg.dll', 'gstopengl.dll',
        'gstopus.dll', 'gstplayback.dll', 'gsttheora.dll', 'gsttypefindfunctions.dll',
        'gstvideoconvertscale.dll', 'gstvolume.dll', 'gstvorbis.dll', 'gstaudiofx.dll',
        'gstaudioparsers.dll', 'gstautodetect.dll', 'gstdeinterlace.dll', 'gstid3demux.dll',
        'gstinterleave.dll', 'gstisomp4.dll', 'gstmatroska.dll', 'gstrtp.dll',
        'gstrtpmanager.dll', 'gstvideofilter.dll', 'gstvpx.dll', 'gstwavparse.dll',
        'gstaudiobuffersplit.dll', 'gstdtls.dll', 'gstid3tag.dll', 'gstproxy.dll',
        'gstvideoparsersbad.dll', 'gstwebrtc.dll', 'gstlibav.dll', 'gstwasapi.dll'
    )
    $pluginDirs = @(
        (Join-Path $gstRoot 'lib\gstreamer-1.0'),
        $gstBin,
        $gstRoot
    ) | Where-Object { Test-Path -LiteralPath $_ -PathType Container } | Select-Object -Unique

    $missingPlugins = [System.Collections.Generic.List[string]]::new()
    $pluginFiles = @(foreach ($pluginName in $requiredPlugins) {
        $pluginFile = $null
        foreach ($pluginDir in $pluginDirs) {
            $candidate = Join-Path $pluginDir $pluginName
            if (Test-Path -LiteralPath $candidate -PathType Leaf) {
                $pluginFile = Get-Item -LiteralPath $candidate
                break
            }
        }
        if ($null -eq $pluginFile) {
            $missingPlugins.Add($pluginName)
            continue
        }
        $pluginFile
    })

    if ($missingPlugins.Count -gt 0) {
        $missingList = $missingPlugins -join ', '
        Write-Warning "Missing Servo 0.8.0 GStreamer plugin DLLs ($($missingPlugins.Count)/$($requiredPlugins.Count)): $missingList"
        if (-not $AllowIncompleteGStreamer) {
            throw 'GStreamer packaging is incomplete. Install the missing plugins or pass -AllowIncompleteGStreamer to continue.'
        }
    }
}

$OutputDir = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($OutputDir)
$OutputDir = [System.IO.Path]::GetFullPath($OutputDir).TrimEnd('\', '/')
$parent = [System.IO.Path]::GetDirectoryName($OutputDir)
if ([string]::IsNullOrEmpty($parent) -or
    "$repoRoot\".StartsWith("$OutputDir\", [System.StringComparison]::OrdinalIgnoreCase) -or
    "$uiSourceDir\".StartsWith("$OutputDir\", [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "Choose a distribution directory separate from the repository and UI sources: $OutputDir"
}

$existing = Get-Item -LiteralPath $OutputDir -Force -ErrorAction SilentlyContinue
if ($existing -and (-not $existing.PSIsContainer -or
    ($existing.Attributes -band [System.IO.FileAttributes]::ReparsePoint))) {
    throw "OutputDir must be a regular directory: $OutputDir"
}

$nonce = [Guid]::NewGuid().ToString('N')
$stage = Join-Path $parent ".qv-package-$nonce"
$backup = Join-Path $parent ".qv-package-backup-$nonce"
[System.IO.Directory]::CreateDirectory($parent) | Out-Null
New-Item -ItemType Directory -Path $stage | Out-Null
try {
    $uiOutputDir = Join-Path $stage 'ui'
    [System.IO.Directory]::CreateDirectory($uiOutputDir) | Out-Null
    Copy-Item -LiteralPath $ExePath -Destination (Join-Path $stage 'quantumview-servo.exe')
    foreach ($dll in ($runtimeDlls + $pluginFiles)) {
        Copy-Item -LiteralPath $dll.FullName -Destination (Join-Path $stage $dll.Name) -Force
    }

    $copied = 0
    foreach ($pattern in $uiPatterns) {
        $files = @(Get-ChildItem -LiteralPath $uiSourceDir -Filter $pattern -File)
        foreach ($file in $files) {
            Copy-Item -LiteralPath $file.FullName -Destination (Join-Path $uiOutputDir $file.Name)
            $copied++
        }
    }
    if ($copied -eq 0) {
        throw "No UI assets matched for App='$App' in $uiSourceDir"
    }

    $licensePath = Join-Path $repoRoot 'LICENSE'
    if (Test-Path -LiteralPath $licensePath -PathType Leaf) {
        Copy-Item -LiteralPath $licensePath -Destination (Join-Path $stage 'LICENSE')
    }

    if ($existing) {
        [System.IO.Directory]::Move($OutputDir, $backup)
    }
    try {
        [System.IO.Directory]::Move($stage, $OutputDir)
    } catch {
        if ($existing) {
            [System.IO.Directory]::Move($backup, $OutputDir)
        }
        throw
    }
    if ($existing) {
        [System.IO.Directory]::Delete($backup, $true)
    }
} finally {
    if ([System.IO.Directory]::Exists($stage)) {
        [System.IO.Directory]::Delete($stage, $true)
    }
}

if ($runtimeDlls.Count -gt 0) {
    Write-Output "Packaged GStreamer runtime ($($runtimeDlls.Count) runtime DLLs, $($pluginFiles.Count)/$($requiredPlugins.Count) plugins) to $OutputDir"
}
Write-Output "Packaged QuantumView ($App, $copied UI files) to $OutputDir"
