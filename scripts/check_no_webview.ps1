<#
.SYNOPSIS
  Issue #16 acceptance gate: a release binary must not carry a WebView/web
  runtime, and the tree must not carry a web asset bundle. Inspects the built
  binary and the filesystem only -- never Cargo.lock or Cargo.toml, which
  contain unrelated false positives (tauri-winrt-notification, gpui-fast-web).
#>
[CmdletBinding()]
param(
    [string]$Binary = 'target\release\orion-ui.exe'
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

# Case-sensitive in the given casing; short markers also need non-alphanumeric
# boundaries, because binary keyword tables contain `wry` and `CEF` by coincidence.
$markers = @('WebView2Loader.dll', 'Microsoft.Web.WebView2', 'WebView2Environment', 'webkit2gtk', 'libwebkit2gtk', 'WKWebView', 'wry::', 'dioxus::')
$webAssets = @('package.json', 'index.html', 'wwwroot', 'dist', 'public', 'assets/web')

$failures = [System.Collections.Generic.List[string]]::new()

function Test-Marker {
    param([string]$Haystack, [string]$Marker)
    $cmp = [System.StringComparison]::Ordinal
    $idx = $Haystack.IndexOf($Marker, $cmp)
    while ($idx -ge 0) {
        if ($Marker.Length -ge 4) { return $true }
        $beforeOk = ($idx -eq 0) -or -not [char]::IsLetterOrDigit($Haystack[$idx - 1])
        $after = $idx + $Marker.Length
        $afterOk = ($after -ge $Haystack.Length) -or -not [char]::IsLetterOrDigit($Haystack[$after])
        if ($beforeOk -and $afterOk) { return $true }
        $idx = $Haystack.IndexOf($Marker, $idx + 1, $cmp)
    }
    return $false
}

$resolved = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($Binary)
if (-not (Test-Path -LiteralPath $resolved -PathType Leaf)) {
    throw "Binary not found: $resolved. Pass -Binary <path> to an existing orion-ui executable."
}

$bytes = [System.IO.File]::ReadAllBytes($resolved)
$latin1 = [System.Text.Encoding]::GetEncoding(28591).GetString($bytes)
$utf16 = [System.Text.Encoding]::Unicode.GetString($bytes)

foreach ($marker in $markers) {
    if ((Test-Marker $latin1 $marker) -or (Test-Marker $utf16 $marker)) {
        $failures.Add("WebView/web-runtime marker '$marker' found in binary $resolved")
    }
}

$root = (Get-Location).Path
foreach ($asset in $webAssets) {
    $candidate = Join-Path $root $asset
    if (Test-Path -LiteralPath $candidate) {
        $failures.Add("web asset '$asset' exists at $candidate")
    }
}

if ($failures.Count -gt 0) {
    [Console]::Error.WriteLine('no-webview gate FAILED:')
    foreach ($failure in $failures) {
        [Console]::Error.WriteLine("  - $failure")
    }
    exit 1
}

Write-Host "no-webview gate passed: $resolved has no WebView/web-runtime markers and no web asset tree exists."
exit 0
