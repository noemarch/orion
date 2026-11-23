<#
.SYNOPSIS
  Verify the checksum of an Orion release archive and extract it into a
  user-local install directory. Idempotent on re-run.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$ArchivePath,
    [string]$InstallDir = "$env:LOCALAPPDATA\Orion\bin"
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$archive = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($ArchivePath)
$sidecar = "$archive.sha256"

if (-not (Test-Path -LiteralPath $archive -PathType Leaf)) {
    throw "Archive not found: $archive"
}
if (-not (Test-Path -LiteralPath $sidecar -PathType Leaf)) {
    throw "Checksum sidecar not found: $sidecar"
}

$sidecarRaw = (Get-Content -LiteralPath $sidecar -Raw).Trim()
$expectedHash = ($sidecarRaw -split '\s+')[0]

$actualHash = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash

if ($actualHash.ToLowerInvariant() -ne $expectedHash.ToLowerInvariant()) {
    throw "Checksum mismatch for $archive`: expected $expectedHash, got $actualHash"
}

$install = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($InstallDir)
New-Item -ItemType Directory -Path $install -Force | Out-Null

Expand-Archive -LiteralPath $archive -DestinationPath $install -Force

$uiExe = Join-Path $install 'orion-ui.exe'
Write-Host "Installed to: $install"
Write-Host "Launch: $uiExe"
exit 0