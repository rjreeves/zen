param(
    [string] $Database = "postgres"
)

$env:PGPASSWORD = ""
$env:PGPASSFILE = Join-Path $env:TEMP "zen-no-pgpass"
$env:PGCONNECT_TIMEOUT = "5"

psql -w -tA -q -c "select 1" $Database *> $null

if ($LASTEXITCODE -eq 0) {
    [Console]::Write("true")
} else {
    [Console]::Write("false")
}

exit 0
