<#
.SYNOPSIS
    QuantumView Servo real-machine Windows verification suite.

.DESCRIPTION
    Runs static checks (rustfmt, cargo metadata, node --check), all standalone
    Rust unit tests (-D warnings), Win32 OS API live probe, E2E --self-test
    modes, screenshot/transparency/DevTools-RDP/tray+single-instance/live-HTTP
    checks via scripts/verify_live.py, and scripts/package.ps1 packaging smoke tests.

.PARAMETER Build
    Rebuild probes\servo in release mode (--frozen) via VS 2022 DevShell + LLVM
    and fail if quantumview-servo emits compiler warnings.

.PARAMETER Interactive
    After automated checks succeed, print the interactive desktop QA checklist
    and launch command for QuantumView Folio (--workspace).
#>
[CmdletBinding()]
param(
    [switch]$Build,
    [switch]$Interactive,
    [string]$ExePath,
    [string]$WorkDir,
    [int]$Jobs = 2
)

$ErrorActionPreference = 'Stop'
if (Get-Variable -Name PSNativeCommandUseErrorActionPreference -ErrorAction SilentlyContinue) {
    $PSNativeCommandUseErrorActionPreference = $false
}

$repoRoot = Split-Path -Parent $PSScriptRoot
if ([string]::IsNullOrWhiteSpace($ExePath)) {
    $ExePath = Join-Path $repoRoot 'probes\servo\target\release\quantumview-servo.exe'
}
if ([string]::IsNullOrWhiteSpace($WorkDir)) {
    $WorkDir = Join-Path ([System.IO.Path]::GetTempPath()) 'quantumview-verify-windows'
}

function Resolve-Tool {
    param(
        [string]$Name,
        [string[]]$Candidates
    )
    foreach ($candidate in $Candidates) {
        if (-not [string]::IsNullOrWhiteSpace($candidate) -and (Test-Path -LiteralPath $candidate -PathType Leaf)) {
            return $candidate
        }
    }
    $cmd = Get-Command $Name -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($cmd) {
        return $cmd.Source
    }
    throw "Required tool '$Name' was not found."
}

function Assert-NoRunningHost {
    $running = @(Get-Process -Name 'quantumview-servo' -ErrorAction SilentlyContinue)
    if ($running.Count -gt 0) {
        $pids = ($running | ForEach-Object { $_.Id }) -join ', '
        throw "Existing quantumview-servo process is still running (PID: $pids). Stop it before running single-instance tests."
    }
}

function Invoke-CmdLogged {
    param(
        [string]$CommandLine,
        [string]$LogPath
    )
    $parent = Split-Path -Parent $LogPath
    if (-not (Test-Path -LiteralPath $parent)) {
        New-Item -ItemType Directory -Path $parent -Force | Out-Null
    }
    cmd /d /s /c " $CommandLine > `"$LogPath`" 2>&1 "
    return $LASTEXITCODE
}

function Initialize-MsvcEnv {
    param(
        [string]$CargoExe,
        [string]$PythonExe
    )
    if (-not (Get-Command 'link.exe' -ErrorAction SilentlyContinue)) {
        $vsInstall = $null
        $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
        if (Test-Path -LiteralPath $vswhere -PathType Leaf) {
            $vsInstall = (& $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath | Select-Object -First 1)
        }
        if ([string]::IsNullOrWhiteSpace($vsInstall)) {
            $vsInstall = 'C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools'
        }
        $devShellDll = Join-Path $vsInstall 'Common7\Tools\Microsoft.VisualStudio.DevShell.dll'
        if (-not (Test-Path -LiteralPath $devShellDll -PathType Leaf)) {
            throw "VS DevShell DLL not found: $devShellDll"
        }
        Import-Module $devShellDll
        Enter-VsDevShell -VsInstallPath $vsInstall -SkipAutomaticLocation -DevCmdArguments '-arch=amd64 -host_arch=amd64' | Out-Null
    }

    $pyDir = Split-Path -Parent $PythonExe
    $cargoDir = Split-Path -Parent $CargoExe
    $llvmBin = 'C:\Program Files\LLVM\bin'
    $env:PATH = "$llvmBin;$pyDir;$cargoDir;$env:PATH"
    if (Test-Path -LiteralPath $llvmBin) {
        $env:LIBCLANG_PATH = $llvmBin
        $env:CC = 'clang-cl'
        $env:CXX = 'clang-cl'
    }
    Remove-Item Env:PYTHON -ErrorAction SilentlyContinue
    Remove-Item Env:PYTHON3 -ErrorAction SilentlyContinue
}

function Invoke-ReleaseBuild {
    param(
        [string]$CargoExe,
        [string]$ManifestPath,
        [string]$LogPath,
        [int]$ParallelJobs
    )
    $cmd = "`"$CargoExe`" build --release --frozen -j $ParallelJobs --manifest-path `"$ManifestPath`""
    $exitCode = Invoke-CmdLogged -CommandLine $cmd -LogPath $LogPath
    if ($exitCode -ne 0) {
        Get-Content -LiteralPath $LogPath -Tail 60 | Out-Host
        throw "Release build failed (exit=$exitCode). See $LogPath"
    }

    $lines = Get-Content -LiteralPath $LogPath -Encoding UTF8
    $qvMatch = $lines | Select-String 'Compiling quantumview-(host|servo)' | Select-Object -First 1
    if ($qvMatch) {
        $tailLines = $lines[($qvMatch.LineNumber - 1)..($lines.Length - 1)]
        $qvWarnings = @($tailLines | Where-Object { $_ -match '^warning(\[[^\]]+\])?:' })
        if ($qvWarnings.Count -gt 0) {
            $qvWarnings | Out-Host
            throw "quantumview-host / quantumview-servo emitted $($qvWarnings.Count) compiler warning(s)."
        }
    }
    Write-Host '[PASS] Release MSVC build succeeded with 0 warnings in quantumview-host and quantumview-servo' -ForegroundColor Green
}

$toolchainBin = Join-Path $env:USERPROFILE '.rustup\toolchains\stable-x86_64-pc-windows-msvc\bin'
$cargoExe = Resolve-Tool -Name 'cargo' -Candidates @(
    (Join-Path $toolchainBin 'cargo.exe'),
    (Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe')
)
$rustcExe = Resolve-Tool -Name 'rustc' -Candidates @(
    (Join-Path $toolchainBin 'rustc.exe'),
    (Join-Path $env:USERPROFILE '.cargo\bin\rustc.exe')
)
$rustfmtExe = Resolve-Tool -Name 'rustfmt' -Candidates @(
    (Join-Path $toolchainBin 'rustfmt.exe'),
    (Join-Path $env:USERPROFILE '.cargo\bin\rustfmt.exe')
)
$nodeExe = Resolve-Tool -Name 'node' -Candidates @('C:\Program Files\nodejs\node.exe')
$pythonExe = Resolve-Tool -Name 'python' -Candidates @(
    (Join-Path $env:LOCALAPPDATA 'Programs\Python\Python313\python.exe'),
    (Join-Path $env:LOCALAPPDATA 'Programs\Python\Python312\python.exe'),
    (Join-Path $env:LOCALAPPDATA 'Programs\Python\Python311\python.exe')
)

Assert-NoRunningHost
Initialize-MsvcEnv -CargoExe $cargoExe -PythonExe $pythonExe

if (Test-Path -LiteralPath $WorkDir) {
    Remove-Item -LiteralPath $WorkDir -Recurse -Force
}
New-Item -ItemType Directory -Path $WorkDir -Force | Out-Null

$manifestPath = Join-Path $repoRoot 'probes\servo\Cargo.toml'
if ($Build -or -not (Test-Path -LiteralPath $ExePath -PathType Leaf)) {
    Write-Host '=== Phase 0: MSVC Release Build ===' -ForegroundColor Cyan
    Invoke-ReleaseBuild -CargoExe $cargoExe -ManifestPath $manifestPath -LogPath (Join-Path $WorkDir 'build.log') -ParallelJobs $Jobs
}
if (-not (Test-Path -LiteralPath $ExePath -PathType Leaf)) {
    throw "Executable not found: $ExePath"
}
$exeHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $ExePath).Hash
$exeLen = (Get-Item -LiteralPath $ExePath).Length
Write-Host "[INFO] Target: $ExePath ($exeLen bytes, SHA-256=$exeHash)"

$savedClipboard = $null
$hasSavedClipboard = $false
try {
    $savedClipboard = Get-Clipboard -Raw -ErrorAction SilentlyContinue
    if ($null -ne $savedClipboard) {
        $hasSavedClipboard = $true
    }
} catch {
}

try {
    Write-Host '=== Phase 1: Static Checks (rustfmt, cargo metadata, node --check) ===' -ForegroundColor Cyan
    $hostManifestPath = Join-Path $repoRoot 'crates\quantumview-host\Cargo.toml'
    & $cargoExe fmt --check --manifest-path $hostManifestPath
    if ($LASTEXITCODE -ne 0) { throw 'cargo fmt --check failed (quantumview-host)' }
    & $cargoExe fmt --check --manifest-path $manifestPath
    if ($LASTEXITCODE -ne 0) { throw 'cargo fmt --check failed (quantumview-servo)' }
    & $rustfmtExe --edition 2024 --check (Join-Path $PSScriptRoot 'win32_live_probe.rs')
    if ($LASTEXITCODE -ne 0) { throw 'rustfmt --check on scripts/win32_live_probe.rs failed' }
    $metaLog = Join-Path $WorkDir 'cargo-metadata.json'
    $metaExit = Invoke-CmdLogged -CommandLine "`"$cargoExe`" metadata --format-version 1 --locked --offline --no-deps --manifest-path `"$manifestPath`"" -LogPath $metaLog
    if ($metaExit -ne 0) { throw 'cargo metadata --locked --offline --no-deps failed' }
    $jsFiles = Get-ChildItem -Path (Join-Path $repoRoot 'probes\servo\ui') -Filter '*.js' -File
    foreach ($js in $jsFiles) {
        & $nodeExe --check $js.FullName
        if ($LASTEXITCODE -ne 0) { throw "node --check failed for $($js.FullName)" }
    }
    Write-Host "[PASS] Static checks passed ($($jsFiles.Count) JS files, rustfmt, locked metadata)" -ForegroundColor Green

    Write-Host '=== Phase 2: Rust Standalone Unit Tests (-D warnings) ===' -ForegroundColor Cyan
    $unitDir = Join-Path $WorkDir 'unit-tests'
    New-Item -ItemType Directory -Path $unitDir -Force | Out-Null
    $modules = @(
        @{ Name = 'win_platform'; RelPath = 'crates\quantumview-host\src\win_platform.rs' },
        @{ Name = 'unix_platform'; RelPath = 'crates\quantumview-host\src\unix_platform.rs' },
        @{ Name = 'window_state'; RelPath = 'crates\quantumview-host\src\window_state.rs' },
        @{ Name = 'bridge'; RelPath = 'crates\quantumview-host\src\bridge.rs' },
        @{ Name = 'embedder_ui'; RelPath = 'crates\quantumview-host\src\embedder_ui.rs' },
        @{ Name = 'protocol'; RelPath = 'crates\quantumview-host\src\protocol.rs' },
        @{ Name = 'tray'; RelPath = 'crates\quantumview-host\src\tray.rs' },
        @{ Name = 'policy'; RelPath = 'crates\quantumview-host\src\policy.rs' },
        @{ Name = 'workspace'; RelPath = 'probes\servo\src\workspace.rs' },
        @{ Name = 'starter'; RelPath = 'probes\servo\src\starter.rs' },
        @{ Name = 'wheel_scroll'; RelPath = 'probes\servo\vendor\servo-paint\wheel_scroll.rs' }
    )
    $totalPassed = 0
    foreach ($mod in $modules) {
        $srcPath = Join-Path $repoRoot $mod.RelPath
        $outExe = Join-Path $unitDir "$($mod.Name).test.exe"
        $compileLog = Join-Path $unitDir "$($mod.Name).compile.log"
        $cExit = Invoke-CmdLogged -CommandLine "`"$rustcExe`" --edition 2024 --test -D warnings `"$srcPath`" -o `"$outExe`"" -LogPath $compileLog
        if ($cExit -ne 0) {
            Get-Content -LiteralPath $compileLog -Encoding UTF8 | Out-Host
            throw "Unit test compile failed for $($mod.RelPath)"
        }
        $runLog = Join-Path $unitDir "$($mod.Name).run.log"
        $rExit = Invoke-CmdLogged -CommandLine "`"$outExe`"" -LogPath $runLog
        $runOut = Get-Content -LiteralPath $runLog -Encoding UTF8 -Raw
        if ($rExit -ne 0) {
            $runOut | Out-Host
            throw "Unit test execution failed for $($mod.RelPath) (exit=$rExit)"
        }
        if ($runOut -match 'test result: ok\. (\d+) passed; 0 failed') {
            $passed = [int]$Matches[1]
            $totalPassed += $passed
            Write-Host "  [PASS] $($mod.Name) ($passed/$passed)" -ForegroundColor Green
        } else {
            $runOut | Out-Host
            throw "Unexpected test output for $($mod.RelPath)"
        }
    }
    Write-Host "[PASS] All $($modules.Count) unit test modules passed ($totalPassed/$totalPassed)" -ForegroundColor Green

    Write-Host '=== Phase 3: Win32 Native Platform Probe ===' -ForegroundColor Cyan
    $winProbeExe = Join-Path $WorkDir 'win32_live_probe.exe'
    $winProbeLog = Join-Path $WorkDir 'win32_probe_compile.log'
    $wpExit = Invoke-CmdLogged -CommandLine "`"$rustcExe`" --edition 2024 -D warnings `"$(Join-Path $PSScriptRoot 'win32_live_probe.rs')`" -o `"$winProbeExe`"" -LogPath $winProbeLog
    if ($wpExit -ne 0) {
        Get-Content -LiteralPath $winProbeLog -Encoding UTF8 | Out-Host
        throw 'win32_live_probe.rs compile failed'
    }
    $winProbeRunLog = Join-Path $WorkDir 'win32_probe_run.log'
    $winProbeExit = Invoke-CmdLogged -CommandLine "`"$winProbeExe`" `"$(Join-Path $WorkDir 'win32-probe-out')`"" -LogPath $winProbeRunLog
    $winProbeOut = Get-Content -LiteralPath $winProbeRunLog -Encoding UTF8 -Raw
    if ($winProbeExit -ne 0) {
        $winProbeOut | Out-Host
        throw "win32_live_probe.exe failed (exit=$winProbeExit)"
    }
    foreach ($marker in @('WIN32_DPAPI_OK', 'WIN32_CLIPBOARD_TEXT_OK', 'WIN32_CLIPBOARD_HTML_OK', 'WIN32_CLIPBOARD_DIB_OK', 'WIN32_CLIPBOARD_CLEAR_OK', 'WIN32_AUTOSTART_HKCU_OK', 'WIN32_PROTOCOL_CLIENT_HKCU_OK', 'WIN32_PDF_GEN_OK', 'WIN32_CRASH_REPORT_ROTATION_OK', 'WIN32_THEME_DETECT_OK')) {
        if ($winProbeOut -notmatch $marker) {
            $winProbeOut | Out-Host
            throw "Missing Win32 probe marker: $marker"
        }
    }
    Write-Host '[PASS] Win32 native DPAPI/Clipboard/HKCU/PDF/CrashReport/Theme checks passed' -ForegroundColor Green

    Write-Host '=== Phase 4: E2E --self-test Matrix ===' -ForegroundColor Cyan
    $wsDirFile = Join-Path $WorkDir 'ws-selftest-file'
    $wsDirQv = Join-Path $WorkDir 'ws-selftest-qv'

    $selfTests = @(
        @{ Name = 'probe-scroll'; Args = "--probe --self-test scroll"; Expect = 'SCROLL_SELF_TEST_OK y1=40 y2=120 y3=80' },
        @{ Name = 'workspace-file'; Args = "--workspace --workspace-dir `"$wsDirFile`" --self-test workspace"; Expect = 'WORKSPACE_SELF_TEST_OK' },
        @{ Name = 'workspace-reload'; Args = "--workspace --workspace-dir `"$wsDirFile`" --self-test workspace-reload"; Expect = 'WORKSPACE_RELOAD_SELF_TEST_OK' },
        @{ Name = 'workspace-qv-assets'; Args = "--workspace --qv-assets --workspace-dir `"$wsDirQv`" --self-test workspace"; Expect = 'WORKSPACE_SELF_TEST_OK' },
        @{ Name = 'starter-qv-assets'; Args = "--starter --qv-assets --self-test starter"; Expect = 'STARTER_SELF_TEST_OK' }
    )
    foreach ($st in $selfTests) {
        $stLog = Join-Path $WorkDir "selftest-$($st.Name).log"
        $stExit = Invoke-CmdLogged -CommandLine "`"$ExePath`" $($st.Args)" -LogPath $stLog
        $stText = Get-Content -LiteralPath $stLog -Encoding UTF8 -Raw
        if ($stExit -ne 0 -or $stText -notmatch [regex]::Escape($st.Expect)) {
            $stText | Out-Host
            throw "Self-test '$($st.Name)' failed (exit=$stExit)"
        }
        Write-Host "  [PASS] $($st.Name)" -ForegroundColor Green
    }

    Write-Host '=== Phase 5: Screenshots, Transparency, DevTools RDP, Tray/Single-Instance, Live HTTP ===' -ForegroundColor Cyan
    & $pythonExe (Join-Path $PSScriptRoot 'verify_live.py') --exe $ExePath --work-dir (Join-Path $WorkDir 'live')
    if ($LASTEXITCODE -ne 0) {
        throw 'verify_live.py failed'
    }

    Write-Host '=== Phase 6: Package Distribution Smoke Test (scripts/package.ps1) ===' -ForegroundColor Cyan
    $distDir = Join-Path $WorkDir 'dist-quantumview'
    & (Join-Path $PSScriptRoot 'package.ps1') -OutputDir $distDir -ExePath $ExePath | Out-Null
    $distExe = Join-Path $distDir 'quantumview-servo.exe'
    $distWsDir = Join-Path $WorkDir 'ws-dist'
    $distLog = Join-Path $WorkDir 'dist-selftest.log'
    Push-Location $distDir
    try {
        $distExit = Invoke-CmdLogged -CommandLine "`"$distExe`" --workspace --qv-assets --workspace-dir `"$distWsDir`" --self-test workspace" -LogPath $distLog
    } finally {
        Pop-Location
    }
    $distText = Get-Content -LiteralPath $distLog -Encoding UTF8 -Raw
    if ($distExit -ne 0 -or $distText -notmatch 'WORKSPACE_SELF_TEST_OK') {
        $distText | Out-Host
        throw "Packaged executable self-test failed (exit=$distExit)"
    }
    Write-Host "[PASS] Packaged bundle self-test passed ($distDir)" -ForegroundColor Green

    Assert-NoRunningHost
    Write-Host ''
    Write-Host "ALL AUTOMATED WINDOWS VERIFICATION CHECKS PASSED (SHA-256: $exeHash)" -ForegroundColor Green
} finally {
    if ($hasSavedClipboard) {
        try {
            Set-Clipboard -Value $savedClipboard -ErrorAction SilentlyContinue
        } catch {
        }
    }
}

if ($Interactive) {
    Write-Host ''
    Write-Host '=== Interactive Desktop QA Checklist ===' -ForegroundColor Yellow
    Write-Host '1. Custom frameless rounded chrome renders cleanly; titlebar drags window; all 8 edge resize handles work; minimize/maximize/close respond.'
    Write-Host '2. Write / Split / Preview modes switch cleanly; Markdown tables, code blocks, checklists, and Japanese IME input render crisply.'
    Write-Host '3. Drag and drop a .md or .txt file from Explorer to import it into the local workspace.'
    Write-Host '4. Export menu: Copy formatted HTML, Save Markdown copy, Export PDF, Save PNG snapshot.'
    Write-Host '5. Render Lab drawer runs 60-frame render check and plots live frame pacing on 2D canvas.'
    Write-Host '6. System tray icon (--tray) and global hotkey (Ctrl+Alt+F12) toggle window visibility cleanly.'
    Write-Host ''
    Write-Host "Launch command: `"$ExePath`" --workspace --qv-assets --tray --single-instance --hotkey Ctrl+Alt+F12"
}
