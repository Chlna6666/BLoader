param(
    [string]$Configuration = "release",
    [string]$OutputDirectory = "dist"
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$target = Join-Path $root "target\$Configuration\BLoader.dll"

if (-not (Test-Path $target)) {
    throw "BLoader.dll not found: $target"
}

$cargo = Get-Content (Join-Path $root "Cargo.toml") -Raw
if ($cargo -notmatch '(?ms)^\[package\].*?^version\s*=\s*"([^"]+)"') {
    throw "Unable to read package version from Cargo.toml"
}

$version = $Matches[1]
$dist = Join-Path $root $OutputDirectory
$stageName = "BLoader-$version-windows-x64"
$stage = Join-Path $dist $stageName
$archive = Join-Path $dist "$stageName.zip"
$archiveChecksum = "$archive.sha256"

Remove-Item $stage -Recurse -Force -ErrorAction SilentlyContinue
Remove-Item $archive -Force -ErrorAction SilentlyContinue
Remove-Item $archiveChecksum -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Path $stage -Force | Out-Null

$dll = Join-Path $stage "BLoader.dll"
Copy-Item $target $dll -Force

foreach ($file in @("README.md", "LICENSE", "CHANGELOG.md")) {
    $source = Join-Path $root $file
    if (Test-Path $source) {
        Copy-Item $source (Join-Path $stage $file) -Force
    }
}

$dllHash = (Get-FileHash $dll -Algorithm SHA256).Hash.ToLowerInvariant()
"$dllHash  BLoader.dll" | Set-Content (Join-Path $stage "BLoader.dll.sha256") -Encoding utf8NoBOM

$sourceCommit = if ($env:BLOADER_SOURCE_SHA) {
    $env:BLOADER_SOURCE_SHA
} elseif ($env:GITHUB_SHA) {
    $env:GITHUB_SHA
} else {
    "local"
}
$workflowCommit = if ($env:GITHUB_SHA) { $env:GITHUB_SHA } else { $sourceCommit }
$runId = if ($env:GITHUB_RUN_ID) { $env:GITHUB_RUN_ID } else { $null }
$builtAt = [DateTime]::UtcNow.ToString("yyyy-MM-ddTHH:mm:ssZ")

$manifest = [ordered]@{
    name = "BLoader"
    version = $version
    target = "x86_64-pc-windows-msvc"
    profile = $Configuration
    source_commit = $sourceCommit
    workflow_commit = $workflowCommit
    github_run_id = $runId
    built_at_utc = $builtAt
    files = [ordered]@{
        "BLoader.dll" = [ordered]@{
            size = (Get-Item $dll).Length
            sha256 = $dllHash
        }
    }
    xuser_bridge = [ordered]@{
        platform = "Win32 GDK only"
        activation = "authenticated BMCBL process-scoped named pipe only"
        hook = "Microsoft xgameruntime.dll!QueryApiImpl only"
        without_session = "no hook; official Microsoft XUser remains untouched"
        signature = "SHA-256 + ECDSA P-256 Xbox proof-of-possession"
        credential_environment_variables = $false
    }
}
$manifest | ConvertTo-Json -Depth 8 | Set-Content (Join-Path $stage "manifest.json") -Encoding utf8NoBOM

Compress-Archive -Path (Join-Path $stage "*") -DestinationPath $archive -CompressionLevel Optimal
$zipHash = (Get-FileHash $archive -Algorithm SHA256).Hash.ToLowerInvariant()
"$zipHash  $stageName.zip" | Set-Content $archiveChecksum -Encoding utf8NoBOM

Write-Host "Package: $archive"
Write-Host "Package SHA256: $zipHash"
Write-Host "DLL SHA256: $dllHash"
Write-Host "Source commit: $sourceCommit"
