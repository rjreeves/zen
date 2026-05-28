param(
    [ValidateSet("show", "patch", "minor", "major", "release", "beta", "set")]
    [string]$Part = "show",

    [string]$Version,

    [string]$Prerelease = "Beta",

    [switch]$DryRun,

    [switch]$NoCargoCheck
)

$ErrorActionPreference = "Stop"

$RepoRoot = Resolve-Path (Join-Path $PSScriptRoot "..")
$CargoToml = Join-Path $RepoRoot "Cargo.toml"

if (-not (Test-Path -LiteralPath $CargoToml)) {
    throw "Cargo.toml was not found at '$CargoToml'"
}

$Toml = Get-Content -LiteralPath $CargoToml -Raw
$PackageVersionPattern = [regex]'(?ms)(^\[package\]\s.*?^version\s*=\s*")([^"]+)(")'
$Match = $PackageVersionPattern.Match($Toml)

if (-not $Match.Success) {
    throw "Could not find [package] version in Cargo.toml"
}

$CurrentVersion = $Match.Groups[2].Value
$SemverPattern = [regex]'^(?<major>\d+)\.(?<minor>\d+)\.(?<patch>\d+)(?<pre>-[0-9A-Za-z.-]+)?(?<build>\+[0-9A-Za-z.-]+)?$'
$Semver = $SemverPattern.Match($CurrentVersion)

if (-not $Semver.Success) {
    throw "Package version '$CurrentVersion' is not supported. Expected MAJOR.MINOR.PATCH[-prerelease][+build]."
}

$Major = [int]$Semver.Groups["major"].Value
$Minor = [int]$Semver.Groups["minor"].Value
$Patch = [int]$Semver.Groups["patch"].Value
$Pre = $Semver.Groups["pre"].Value
$Build = $Semver.Groups["build"].Value

function Format-Version {
    param(
        [int]$Major,
        [int]$Minor,
        [int]$Patch,
        [string]$Pre,
        [string]$Build
    )

    "$Major.$Minor.$Patch$Pre$Build"
}

switch ($Part) {
    "show" {
        $NewVersion = $CurrentVersion
    }
    "set" {
        if (-not $Version) {
            throw "Use -Version with 'set', e.g. Scripts\bump-version.ps1 set -Version 9.0.0"
        }
        if (-not $SemverPattern.IsMatch($Version)) {
            throw "Version '$Version' is not valid SemVer."
        }
        $NewVersion = $Version
    }
    "patch" {
        $Patch += 1
        $Build = ""
        $NewVersion = Format-Version $Major $Minor $Patch $Pre $Build
    }
    "minor" {
        $Minor += 1
        $Patch = 0
        $Build = ""
        $NewVersion = Format-Version $Major $Minor $Patch $Pre $Build
    }
    "major" {
        $Major += 1
        $Minor = 0
        $Patch = 0
        $Build = ""
        $NewVersion = Format-Version $Major $Minor $Patch $Pre $Build
    }
    "release" {
        $Pre = ""
        $Build = ""
        $NewVersion = Format-Version $Major $Minor $Patch $Pre $Build
    }
    "beta" {
        $Pre = "-$Prerelease"
        $Build = ""
        $NewVersion = Format-Version $Major $Minor $Patch $Pre $Build
    }
}

Write-Host "Current version: $CurrentVersion"
Write-Host "New version    : $NewVersion"

if ($Part -eq "show" -or $NewVersion -eq $CurrentVersion) {
    if ($Part -eq "show") {
        exit 0
    }

    Write-Host "No change needed."
    exit 0
}

if ($DryRun) {
    Write-Host "Dry run: Cargo.toml was not changed."
    exit 0
}

$UpdatedToml = $PackageVersionPattern.Replace(
    $Toml,
    { param($m) $m.Groups[1].Value + $NewVersion + $m.Groups[3].Value },
    1
)

Set-Content -LiteralPath $CargoToml -Value $UpdatedToml -NoNewline
Write-Host "Updated Cargo.toml"

if (-not $NoCargoCheck) {
    Push-Location -LiteralPath $RepoRoot
    try {
        cargo check
    } finally {
        Pop-Location
    }
}

