$ErrorActionPreference = "Stop"

$refreshToken = Read-Host "DROPBOX_REFRESH_TOKEN"
$appKey = Read-Host "DROPBOX_APP_KEY"
$appSecret = Read-Host "DROPBOX_APP_SECRET"

try {
    $env:DROPBOX_REFRESH_TOKEN = $refreshToken
    $env:DROPBOX_APP_KEY = $appKey
    $env:DROPBOX_APP_SECRET = $appSecret

    zen run examples/dropbox-env-to-secrets.fg --yes
}
finally {
    Remove-Item Env:DROPBOX_REFRESH_TOKEN -ErrorAction SilentlyContinue
    Remove-Item Env:DROPBOX_APP_KEY -ErrorAction SilentlyContinue
    Remove-Item Env:DROPBOX_APP_SECRET -ErrorAction SilentlyContinue
}
