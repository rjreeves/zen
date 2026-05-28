$raw = & schtasks /Query /FO CSV /V 2>&1

if ($LASTEXITCODE -ne 0) {
    [pscustomobject]@{
        success = $false
        exit_code = $LASTEXITCODE
        error = (($raw | Out-String).Trim())
        tasks = @()
    } | ConvertTo-Json -Depth 6
    exit 0
}

[pscustomobject]@{
    success = $true
    exit_code = 0
    error = $null
    tasks = @($raw | ConvertFrom-Csv)
} | ConvertTo-Json -Depth 8
