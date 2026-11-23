<#
.SYNOPSIS
  Package a versioned, target-named release ZIP for Orion.

  Creates OutDir\orion-<Version>-<Os>.zip containing orion, orion-ui and
  orion-rt (with .exe suffix on Windows targets) at the archive root, plus a
  <zip>.sha256 sidecar in the form "<lowercase sha256>  <zipfilename>".
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$Version,
    [Parameter(Mandatory = $true)][string]$Os,
    [Parameter(Mandatory = $true)][string]$BinaryDir,
    [Parameter(Mandatory = $true)][string]$OutDir
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$suffix = if ($Os -like 'windows*') { '.exe' } else { '' }
$names = @('orion', 'orion-ui', 'orion-rt')

$binDir = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($BinaryDir)
$outDir = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($OutDir)

if (-not (Test-Path -LiteralPath $binDir -PathType Container)) {
    throw "BinaryDir not found: $binDir"
}

New-Item -ItemType Directory -Path $outDir -Force | Out-Null

foreach ($n in $names) {
    $src = Join-Path $binDir "$n$suffix"
    if (-not (Test-Path -LiteralPath $src -PathType Leaf)) {
        throw "Missing source binary: $src"
    }
}

$zipName = "orion-$Version-$Os.zip"
$zipPath = Join-Path $outDir $zipName

$staging = Join-Path ([System.IO.Path]::GetTempPath()) ("orion-pkg-" + [Guid]::NewGuid().ToString())
New-Item -ItemType Directory -Path $staging -Force | Out-Null
try {
    foreach ($n in $names) {
        Copy-Item -LiteralPath (Join-Path $binDir "$n$suffix") -Destination (Join-Path $staging "$n$suffix") -Force
    }
    if (Test-Path -LiteralPath $zipPath) {
        Remove-Item -LiteralPath $zipPath -Force
    }
    Compress-Archive -Path (Join-Path $staging '*') -DestinationPath $zipPath -Force
} finally {
    Remove-Item -LiteralPath $staging -Recurse -Force -ErrorAction SilentlyContinue
}

$hash = (Get-FileHash -LiteralPath $zipPath -Algorithm SHA256).Hash.ToLowerInvariant()
$sidecar = "$zipPath.sha256"
Set-Content -LiteralPath $sidecar -Value "$hash  $zipName"

Write-Host "Packaged $zipPath"
exit 0