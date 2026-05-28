param(
    [Parameter(Mandatory = $false)]
    [ValidateRange(1, 365)]
    [int] $OlderThanDays = 7,

    [Parameter(Mandatory = $false)]
    [switch] $DryRun
)

$ErrorActionPreference = "Stop"

$expectedTemp = Join-Path $env:LOCALAPPDATA "Temp"
$tempPath = [System.IO.Path]::GetFullPath($expectedTemp)

if (-not (Test-Path -LiteralPath $tempPath -PathType Container)) {
    throw "Temp folder does not exist: $tempPath"
}

$resolvedTemp = (Resolve-Path -LiteralPath $tempPath).Path.TrimEnd("\")
$resolvedExpected = (Resolve-Path -LiteralPath $expectedTemp).Path.TrimEnd("\")

if ($resolvedTemp -ne $resolvedExpected -or $resolvedTemp -notlike "$env:USERPROFILE\AppData\Local\Temp") {
    throw "Refusing to clean unexpected path: $resolvedTemp"
}

$cutoff = (Get-Date).AddDays(-$OlderThanDays)
$deletedFiles = 0
$deletedFolders = 0
$skipped = 0

function Remove-OldItem {
    param(
        [Parameter(Mandatory = $true)]
        [System.IO.FileSystemInfo] $Item
    )

    if ($DryRun) {
        Write-Host "Would delete: $($Item.FullName)"
        return
    }

    Remove-Item -LiteralPath $Item.FullName -Recurse -Force -ErrorAction Stop
}

function Test-DirectoryIsOlderThanCutoff {
    param(
        [Parameter(Mandatory = $true)]
        [System.IO.DirectoryInfo] $Directory
    )

    if ($Directory.LastWriteTime -ge $cutoff) {
        return $false
    }

    $newerChild = Get-ChildItem -LiteralPath $Directory.FullName -Recurse -Force -ErrorAction SilentlyContinue |
        Where-Object { $_.LastWriteTime -ge $cutoff } |
        Select-Object -First 1

    return $null -eq $newerChild
}

Write-Host "Cleaning files and folders older than $OlderThanDays days from: $resolvedTemp"
Write-Host "Cutoff: $cutoff"

Get-ChildItem -LiteralPath $resolvedTemp -Force -File -ErrorAction SilentlyContinue |
    Where-Object { $_.LastWriteTime -lt $cutoff } |
    ForEach-Object {
        $itemPath = $_.FullName
        try {
            Remove-OldItem -Item $_
            $deletedFiles++
        } catch {
            $skipped++
            Write-Warning "Skipped file '$itemPath': $($_.Exception.Message)"
        }
    }

Get-ChildItem -LiteralPath $resolvedTemp -Force -Directory -ErrorAction SilentlyContinue |
    ForEach-Object {
        $itemPath = $_.FullName
        try {
            if (Test-DirectoryIsOlderThanCutoff -Directory $_) {
                Remove-OldItem -Item $_
                $deletedFolders++
            }
        } catch {
            $skipped++
            Write-Warning "Skipped folder '$itemPath': $($_.Exception.Message)"
        }
    }

if ($DryRun) {
    Write-Host "Dry run complete."
} else {
    Write-Host "Deleted files: $deletedFiles"
    Write-Host "Deleted folders: $deletedFolders"
    Write-Host "Skipped locked or inaccessible items: $skipped"
}
