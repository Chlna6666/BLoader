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
$stage = Join-Path $root "$OutputDirectory\BLoader-$version-windows-x64"
$archive = "$stage.zip"

Remove-Item $stage -Recurse -Force -ErrorAction SilentlyContinue
Remove-Item $archive -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Path $stage -Force | Out-Null

Copy-Item $target (Join-Path $stage "BLoader.dll")
foreach ($file in @("README.md", "LICENSE", "CHANGELOG.md")) {
    $source = Join-Path $root $file
    if (Test-Path $source) {
        Copy-Item $source (Join-Path $stage $file)
    }
}

$manifest = [ordered]@{
    name = "BLoader"
    version = $version
    target = "x86_64-pc-windows-msvc"
    profile = $Configuration
    sha256 = (Get-FileHash (Join-Path $stage "BLoader.dll") -Algorithm SHA256).Hash.ToLowerInvariant()
    xuser_bridge = [ordered]@{
        activation = "authenticated BMCBL named pipe only"
        hook = "xgameruntime.dll!QueryApiImpl only"
        default_without_session = "official Microsoft XUser untouched"
        signature = "SHA-256 + ECDSA P-256 Xbox proof-of-possession"
    }
}
$manifest | ConvertTo-Json -Depth 5 | Set-Content (Join-Path $stage "manifest.json") -Encoding UTF8

Compress-Archive -Path (Join-Path $stage "*") -DestinationPath $archive -CompressionLevel Optimal
Write-Host "Package: $archive"
Write-Host "SHA256: $((Get-FileHash $archive -Algorithm SHA256).Hash.ToLowerInvariant())"
