# dropbox-client-status.ps1
# Read-only check for the Dropbox desktop client on Windows.

$ErrorActionPreference = "Stop"

$candidatePaths = @(
    (Join-Path $env:LOCALAPPDATA "Dropbox\Client\Dropbox.exe"),
    (Join-Path $env:ProgramFiles "Dropbox\Client\Dropbox.exe")
)

if ($env:ProgramFiles -ne ${env:ProgramFiles(x86)} -and ${env:ProgramFiles(x86)}) {
    $candidatePaths += (Join-Path ${env:ProgramFiles(x86)} "Dropbox\Client\Dropbox.exe")
}

$installPath = $candidatePaths |
    Where-Object { $_ -and (Test-Path -LiteralPath $_ -PathType Leaf) } |
    Select-Object -First 1

$processes = @(Get-Process -Name "Dropbox" -ErrorAction SilentlyContinue)

Write-Output "DROPBOX CLIENT STATUS"
Write-Output "---------------------"
Write-Output ("Installed : {0}" -f ($(if ($installPath) { "yes" } else { "no" })))
Write-Output ("Path      : {0}" -f ($(if ($installPath) { $installPath } else { "(not found)" })))
Write-Output ("Running   : {0}" -f ($(if ($processes.Count -gt 0) { "yes" } else { "no" })))
Write-Output ("Processes : {0}" -f $processes.Count)

if ($processes.Count -gt 0) {
    Write-Output ""
    $processes |
        Sort-Object Id |
        Select-Object Name, Id, Path, StartTime |
        Format-Table -AutoSize |
        Out-String -Width 240 |
        Write-Output
}
