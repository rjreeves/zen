New-Item -ItemType Directory -Force .zen | Out-Null

$json = & "$PSScriptRoot\tasks-json.ps1"
$target = Join-Path (Get-Location) ".zen/schedules.json"
$encoding = New-Object System.Text.UTF8Encoding $false
[System.IO.File]::WriteAllText($target, ($json -join [Environment]::NewLine), $encoding)

Get-Item .zen/schedules.json |
    Select-Object FullName, Length |
    ConvertTo-Json
