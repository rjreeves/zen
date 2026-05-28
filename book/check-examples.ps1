param(
    [string]$ZenPath,
    [switch]$RunHarmless
)

$ErrorActionPreference = "Stop"

$RepoRoot = Resolve-Path (Join-Path $PSScriptRoot "..")
$Examples = Join-Path $PSScriptRoot "examples"

if (-not $ZenPath) {
    $Candidates = @(
        (Join-Path $RepoRoot "target\debug\zen.exe"),
        (Join-Path $RepoRoot "target\release\zen.exe"),
        (Join-Path $RepoRoot "dist\zen.exe"),
        "zen"
    )

    foreach ($Candidate in $Candidates) {
        if ($Candidate -eq "zen") {
            $Command = Get-Command zen -ErrorAction SilentlyContinue
            if ($Command) {
                $ZenPath = $Command.Source
                break
            }
            continue
        }

        if (Test-Path -LiteralPath $Candidate) {
            $ZenPath = $Candidate
            break
        }
    }
}

if (-not $ZenPath) {
    throw "Could not find zen. Pass -ZenPath or build target\debug\zen.exe."
}

Write-Host "Zen:" $ZenPath
Write-Host "Examples:" $Examples
Write-Host ""

Push-Location -LiteralPath $RepoRoot

$Checks = New-Object System.Collections.Generic.List[object]

function Add-Check {
    param(
        [string]$Name,
        [string]$Kind,
        [bool]$Passed,
        [string]$Detail
    )

    $Checks.Add([pscustomobject]@{
        Name = $Name
        Kind = $Kind
        Passed = $Passed
        Detail = $Detail
    })
}

function Invoke-Zen {
    param([string[]]$Arguments)

    $PreviousErrorActionPreference = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    $Output = & $ZenPath @Arguments 2>&1
    $ExitCode = $LASTEXITCODE
    $ErrorActionPreference = $PreviousErrorActionPreference

    [pscustomobject]@{
        ExitCode = $ExitCode
        Output = ($Output -join [Environment]::NewLine)
    }
}

$ExampleFiles = Get-ChildItem -LiteralPath $Examples -File |
    Where-Object { $_.Extension -in @(".zen", ".fg", ".yaml", ".yml") } |
    Sort-Object Name

foreach ($File in $ExampleFiles) {
    $Relative = Resolve-Path -Relative $File.FullName
    $Result = Invoke-Zen -Arguments @("explain", $Relative)
    Add-Check $File.Name "explain" ($Result.ExitCode -eq 0) $Result.Output
}

if ($RunHarmless) {
    foreach ($Name in @("01-hello.zen", "02-permissions.zen")) {
        $Path = Join-Path $Examples $Name
        $Relative = Resolve-Path -Relative $Path
        $Args = @("run", $Relative)
        if ($Name -ne "01-hello.zen") {
            $Args += "--yes"
        }
        $Result = Invoke-Zen -Arguments $Args
        Add-Check $Name "run" ($Result.ExitCode -eq 0) $Result.Output
    }
}

$Failures = @($Checks | Where-Object { -not $_.Passed })

foreach ($Check in $Checks) {
    $Status = if ($Check.Passed) { "PASS" } else { "FAIL" }
    Write-Host ("[{0}] {1} {2}" -f $Status, $Check.Kind, $Check.Name)
    if (-not $Check.Passed -and $Check.Detail) {
        Write-Host $Check.Detail
    }
}

Write-Host ""
Write-Host ("Summary: {0} checks, {1} passed, {2} failed" -f $Checks.Count, ($Checks.Count - $Failures.Count), $Failures.Count)

Pop-Location

if ($Failures.Count -gt 0) {
    exit 1
}
