<#
.SYNOPSIS
  Acceptance gate: every named test must exist. Fails when a test filter named
  in an acceptance criterion matches zero tests in the crate.

.DESCRIPTION
  `cargo test -p <crate> -- <filter>` exits 0 when the filter matches nothing,
  so a criterion that names a typo'd or moved test (for example
  `sidebar::tests::titlebar_shows_warning_when_offline`, or the module path
  `runtime::tests` instead of `runtime::client::tests`) silently "passes" with
  "0 tests run". This script lists the crate's tests first and fails if any
  named filter matches none of them.

  Filters are matched as case-sensitive substrings of the listed test names, so
  `runtime::client::tests` selects that module and a full path selects exactly
  one test. Run it from the repository root.

.PARAMETER Crate
  Cargo package name, for example `orion-ui` or `orion-core`.

.PARAMETER Filter
  One or more test filters that must each match at least one listed test.

.PARAMETER TestType
  `lib` (default) lists unit tests from the crate's library target with
  `cargo test -p <crate> --lib -- --list`. `all` lists every target with
  `cargo test -p <crate> -- --list`.

.EXAMPLE
  ./scripts/check-named-tests.ps1 -Crate orion-ui -Filter 'composer::tests::affordance_controls_render_at_touch_target_height'

  Exits 0 when that test exists.

.EXAMPLE
  ./scripts/check-named-tests.ps1 -Crate orion-core -Filter 'runtime::tests'

  Exits 1 with "matched 0 tests" when no test name contains that substring.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true, Position = 0)]
    [string]$Crate,

    [Parameter(Mandatory = $true, Position = 1, ValueFromRemainingArguments = $true)]
    [string[]]$Filter,

    [ValidateSet('lib', 'all')]
    [string]$TestType = 'lib'
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

if (-not (Test-Path -LiteralPath 'Cargo.toml' -PathType Leaf)) {
    throw "Run this script from the repository root; no Cargo.toml in $((Get-Location).Path)."
}

$cargoArgs = @('test', '-p', $Crate)
if ($TestType -eq 'lib') { $cargoArgs += '--lib' }
$cargoArgs += @('--', '--list')

$listing = & cargo @cargoArgs 2>&1
$cargoExit = $LASTEXITCODE
$listingLines = @($listing | ForEach-Object { $_.ToString() })

if ($cargoExit -ne 0) {
    [Console]::Error.WriteLine("named-tests gate FAILED: 'cargo $($cargoArgs -join ' ')' exited $cargoExit")
    foreach ($line in $listingLines) { [Console]::Error.WriteLine("  | $line") }
    exit 1
}

# `--list` prints one `name: test` line per test, then a `N tests, M benchmarks` summary.
$testNames = [System.Collections.Generic.List[string]]::new()
foreach ($line in $listingLines) {
    if ($line -match '^(?<name>.+): test$') { $testNames.Add($Matches['name']) }
}

if ($testNames.Count -eq 0) {
    [Console]::Error.WriteLine("named-tests gate FAILED: 'cargo $($cargoArgs -join ' ')' listed no tests for crate '$Crate'.")
    exit 1
}

# `pwsh -File` passes `-Filter 'a','b'` through as one literal string, so split
# on commas and drop stray quotes to keep both invocation styles working.
$patterns = [System.Collections.Generic.List[string]]::new()
foreach ($entry in $Filter) {
    foreach ($piece in $entry.Split(',')) {
        $trimmed = $piece.Trim().Trim("'", '"').Trim()
        if ($trimmed.Length -gt 0) { $patterns.Add($trimmed) }
    }
}

if ($patterns.Count -eq 0) {
    [Console]::Error.WriteLine('named-tests gate FAILED: no non-empty -Filter value was supplied.')
    exit 1
}

$failures = [System.Collections.Generic.List[string]]::new()
foreach ($pattern in $patterns) {
    $matches = @($testNames | Where-Object { $_.Contains($pattern, [System.StringComparison]::Ordinal) })
    if ($matches.Count -eq 0) {
        $failures.Add("filter '$pattern' matched 0 tests in crate '$Crate' ($($testNames.Count) listed)")
    } else {
        Write-Host "filter '$pattern' matched $($matches.Count) test(s) in $Crate"
    }
}

if ($failures.Count -gt 0) {
    [Console]::Error.WriteLine('named-tests gate FAILED:')
    foreach ($failure in $failures) { [Console]::Error.WriteLine("  - $failure") }
    exit 1
}

Write-Host "named-tests gate passed: $($patterns.Count) filter(s) matched in $Crate."
exit 0
