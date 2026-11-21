# Install tinybrowser from GitHub Releases on Windows.
#
# Environment overrides:
#   TINYBROWSER_VERSION      Tag to install (e.g. "0.0.1" or "v0.0.1"). Default: latest release.
#   TINYBROWSER_INSTALL_DIR  Destination directory. Default: $env:LOCALAPPDATA\tinybrowser\bin.

$ErrorActionPreference = "Stop"

$Repo = "tinybrowser-dev/tinybrowser"
$Target = "x86_64-pc-windows-msvc"

if ([System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture -ne [System.Runtime.InteropServices.Architecture]::X64) {
    throw "No prebuilt Windows binary for $([System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture) yet (supported: x86_64). Build from source: https://github.com/$Repo#building-from-source"
}

function Resolve-Version {
    if ($env:TINYBROWSER_VERSION) {
        return $env:TINYBROWSER_VERSION.TrimStart("v")
    }
    $response = Invoke-WebRequest -Uri "https://github.com/$Repo/releases/latest" -MaximumRedirection 0 -ErrorAction SilentlyContinue
    $location = $response.Headers["Location"]
    if (-not $location) {
        $final = Invoke-WebRequest -Uri "https://github.com/$Repo/releases/latest" -UseBasicParsing
        $location = $final.BaseResponse.ResponseUri.AbsoluteUri
    }
    $tag = ($location.TrimEnd("/") -split "/")[-1]
    if (-not $tag.StartsWith("v")) {
        throw "Could not determine latest release tag from $location"
    }
    return $tag.Substring(1)
}

$version = Resolve-Version
$tag = "v$version"
$archive = "tinybrowser-$version-$Target.zip"
$baseUrl = "https://github.com/$Repo/releases/download/$tag"
$installDir = if ($env:TINYBROWSER_INSTALL_DIR) {
    $env:TINYBROWSER_INSTALL_DIR
} else {
    Join-Path $env:LOCALAPPDATA "tinybrowser\bin"
}

$tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("tinybrowser-install-" + [System.Guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Force -Path $tmp | Out-Null

try {
    $archivePath = Join-Path $tmp $archive
    $sumsPath = Join-Path $tmp "sha256sums.txt"

    Write-Host "Downloading tinybrowser $version ($Target)..."
    Invoke-WebRequest -Uri "$baseUrl/$archive" -OutFile $archivePath -UseBasicParsing
    Invoke-WebRequest -Uri "$baseUrl/sha256sums.txt" -OutFile $sumsPath -UseBasicParsing

    $expectedLine = Get-Content $sumsPath | Where-Object { $_ -match "\s$([regex]::Escape($archive))$" } | Select-Object -First 1
    if (-not $expectedLine) {
        throw "Checksum entry not found for $archive"
    }
    $expected = ($expectedLine -split "\s+")[0].ToLowerInvariant()
    $actual = (Get-FileHash -Path $archivePath -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($actual -ne $expected) {
        throw "Checksum mismatch for $archive (expected $expected, got $actual)"
    }

    Expand-Archive -Path $archivePath -DestinationPath $tmp -Force
    New-Item -ItemType Directory -Force -Path $installDir | Out-Null
    Copy-Item -Path (Join-Path $tmp "tinybrowser-$version-$Target\tinybrowser.exe") -Destination (Join-Path $installDir "tinybrowser.exe") -Force

    $exe = Join-Path $installDir "tinybrowser.exe"
    Write-Host "Installed $exe"
    & $exe --version

    $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
    $pathEntries = if ($userPath) { $userPath -split ";" } else { @() }
    if ($pathEntries -notcontains $installDir) {
        Write-Host "Note: add $installDir to your PATH to run 'tinybrowser' directly."
    }
} finally {
    Remove-Item -Recurse -Force -Path $tmp -ErrorAction SilentlyContinue
}
