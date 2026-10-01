param(
    [switch]$PrintPlan
)

$ErrorActionPreference = "Stop"

if ($PrintPlan) {
    Write-Output "Windows: native x86_64-pc-windows-msvc runtime"
    Write-Output "bound: 1200 seconds"
    Write-Output "npm executable: npm.cmd"
    Write-Output "raw log: evidence/raw/phase2/plugin-platform-windows.log"
    exit 0
}

$RepositoryRoot = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
$ExpectedBaseline = "5de45e208690b0efc51c59a585ae9729325a9204"
$BaselineRoot = Join-Path $RepositoryRoot ".baselines\paseo-runtime"
$RawDirectory = Join-Path $RepositoryRoot "evidence\raw\phase2"
$LogFile = Join-Path $RawDirectory "plugin-platform-windows.log"
$StdoutFile = Join-Path $env:TEMP "spocky-plugin-platform-$PID.stdout.log"
$StderrFile = Join-Path $env:TEMP "spocky-plugin-platform-$PID.stderr.log"
$ExpectedTests = 25
New-Item -ItemType Directory -Force -Path $RawDirectory | Out-Null

$ActualBaseline = (& git.exe -C $BaselineRoot rev-parse HEAD).Trim()
if ($ActualBaseline -ne $ExpectedBaseline) {
    throw "Paseo baseline mismatch: expected $ExpectedBaseline, got $ActualBaseline"
}
if ((& git.exe -C $BaselineRoot status --porcelain).Count -ne 0) {
    throw "Paseo baseline is dirty: $BaselineRoot"
}

try {
    @(
        "platform: $([System.Environment]::OSVersion.VersionString)"
        "rustc: $(& rustc.exe --version)"
        "cargo: $(& cargo.exe --version)"
        "git: $(& git.exe --version)"
        "node: $(& node.exe --version)"
        "npm: $(& npm.cmd --version)"
    ) | Set-Content -Encoding UTF8 $LogFile

    $Arguments = @(
        "test", "--locked", "-p", "spocky-plugin-pilot",
        "--test", "runtime_acquisition",
        "--test", "selected_server_runtime",
        "--test", "client_runtime",
        "--test", "client_contribution_runtime",
        "--test", "settings_lifecycle",
        "--", "--test-threads=1"
    )
    $Process = Start-Process -FilePath "cargo.exe" -ArgumentList $Arguments `
        -WorkingDirectory $RepositoryRoot -NoNewWindow -PassThru `
        -RedirectStandardOutput $StdoutFile -RedirectStandardError $StderrFile
    if (-not $Process.WaitForExit(1200000)) {
        & taskkill.exe /PID $Process.Id /T /F | Out-Null
        throw "Windows plugin qualification timed out after 1200 seconds"
    }
    Get-Content $StdoutFile, $StderrFile | Add-Content -Encoding UTF8 $LogFile
    if ($Process.ExitCode -ne 0) {
        throw "Windows plugin qualification failed with exit code $($Process.ExitCode)"
    }
    $ActualTests = (Select-String -Path $LogFile -Pattern '^test .* \.\.\. ok$').Count
    if ($ActualTests -ne $ExpectedTests) {
        throw "Windows qualification executed $ActualTests passing tests, expected $ExpectedTests"
    }
    Add-Content -Encoding UTF8 $LogFile "PLUGIN_PLATFORM_WINDOWS_OK"
    $Digest = (Get-FileHash -Algorithm SHA256 $LogFile).Hash.ToLowerInvariant()
    Write-Output "Windows plugin qualification passed: $LogFile"
    Write-Output "$Digest  $LogFile"
}
finally {
    Remove-Item -Force -ErrorAction SilentlyContinue $StdoutFile, $StderrFile
}
