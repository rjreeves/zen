param(
    [string]$Path = ".\zen-default.credentials.encrypted.json",

    [string]$Passphrase="o",

    [string]$PassphraseEnvVar = "DROPBOX_CREDENTIAL_EXPORT_PASSPHRASE",

    [string]$TargetPrefix = "zen/default",

    [string]$UserName,

    [switch]$PreserveTargets,

    [switch]$CompareOnly,

    [switch]$TestOnly
)

$ErrorActionPreference = "Stop"

if ($PSVersionTable.PSVersion.Major -lt 7) {
    $pwshCandidates = @(
        (Get-Command pwsh.exe -ErrorAction SilentlyContinue | Select-Object -ExpandProperty Source -First 1),
        "$env:ProgramFiles\PowerShell\7\pwsh.exe",
        "${env:ProgramFiles(x86)}\PowerShell\7\pwsh.exe",
        "$env:LOCALAPPDATA\Microsoft\powershell\pwsh.exe"
    ) | Where-Object { -not [string]::IsNullOrWhiteSpace($_) } | Select-Object -Unique

    $pwsh = $pwshCandidates | Where-Object { Test-Path -LiteralPath $_ } | Select-Object -First 1
    if (-not $pwsh) {
        throw "This encrypted JSON importer requires PowerShell 7+. pwsh.exe was not found on PATH or in the standard install folders."
    }

    $relaunchArgs = @(
        "-NoProfile",
        "-ExecutionPolicy", "Bypass",
        "-File", $PSCommandPath,
        "-Path", $Path,
        "-PassphraseEnvVar", $PassphraseEnvVar,
        "-TargetPrefix", $TargetPrefix
    )

    if (-not [string]::IsNullOrWhiteSpace($Passphrase)) {
        $relaunchArgs += @("-Passphrase", $Passphrase)
    }

    if (-not [string]::IsNullOrWhiteSpace($UserName)) {
        $relaunchArgs += @("-UserName", $UserName)
    }

    if ($PreserveTargets) {
        $relaunchArgs += "-PreserveTargets"
    }

    if ($CompareOnly) {
        $relaunchArgs += "-CompareOnly"
    }

    if ($TestOnly) {
        $relaunchArgs += "-TestOnly"
    }

    & $pwsh @relaunchArgs
    exit $LASTEXITCODE
}

function Add-CredentialManagerType {
    if ("CredentialManager.NativeMethods" -as [type]) {
        return
    }

    Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;

namespace CredentialManager
{
    public enum CredType : uint
    {
        Generic = 1
    }

    public enum CredPersist : uint
    {
        LocalMachine = 2
    }

    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    public struct Credential
    {
        public uint Flags;
        public CredType Type;
        public string TargetName;
        public string Comment;
        public System.Runtime.InteropServices.ComTypes.FILETIME LastWritten;
        public uint CredentialBlobSize;
        public IntPtr CredentialBlob;
        public CredPersist Persist;
        public uint AttributeCount;
        public IntPtr Attributes;
        public string TargetAlias;
        public string UserName;
    }

    public static class NativeMethods
    {
        [DllImport("advapi32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        public static extern bool CredRead(string target, CredType type, int reservedFlag, out IntPtr credentialPtr);

        [DllImport("advapi32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        public static extern bool CredWrite(ref Credential userCredential, uint flags);

        [DllImport("advapi32.dll", SetLastError = true)]
        public static extern void CredFree(IntPtr buffer);
    }
}
"@
}

function Get-WindowsCredentialPassword {
    param([Parameter(Mandatory)][string]$Target)

    Add-CredentialManagerType
    $credentialPtr = [IntPtr]::Zero
    $ok = [CredentialManager.NativeMethods]::CredRead($Target, [CredentialManager.CredType]::Generic, 0, [ref]$credentialPtr)
    if (-not $ok) {
        return $null
    }

    try {
        $credential = [Runtime.InteropServices.Marshal]::PtrToStructure($credentialPtr, [type][CredentialManager.Credential])
        if ($credential.CredentialBlobSize -eq 0) {
            return ""
        }

        $bytes = New-Object byte[] $credential.CredentialBlobSize
        [Runtime.InteropServices.Marshal]::Copy($credential.CredentialBlob, $bytes, 0, $bytes.Length)
        ConvertFrom-CredentialBlob $bytes
    }
    finally {
        [CredentialManager.NativeMethods]::CredFree($credentialPtr)
    }
}

function Clean-Secret {
    param([string]$Value)

    if ([string]::IsNullOrWhiteSpace($Value)) {
        return ""
    }

    $clean = $Value.Trim()
    $clean = $clean.Trim('"').Trim("'").Trim()
    return $clean
}

function Test-AsciiSecret {
    param([string]$Value)

    if ([string]::IsNullOrEmpty($Value)) {
        return $false
    }

    foreach ($char in $Value.ToCharArray()) {
        $code = [int][char]$char
        if ($code -lt 33 -or $code -gt 126) {
            return $false
        }
    }

    return $true
}

function ConvertFrom-CredentialBlob {
    param([byte[]]$Bytes)

    $unicode = ([Text.Encoding]::Unicode.GetString($Bytes)).TrimEnd([char]0)
    $utf8 = ([Text.Encoding]::UTF8.GetString($Bytes)).TrimEnd([char]0)

    $unicodeClean = Clean-Secret $unicode
    $utf8Clean = Clean-Secret $utf8

    if (Test-AsciiSecret $utf8Clean) {
        return $utf8Clean
    }

    if (Test-AsciiSecret $unicodeClean) {
        return $unicodeClean
    }

    return $unicodeClean
}

function Set-WindowsCredentialPassword {
    param(
        [Parameter(Mandatory)][string]$Target,
        [Parameter(Mandatory)][string]$UserName,
        [Parameter(Mandatory)][string]$Password
    )

    Add-CredentialManagerType
    $bytes = [Text.Encoding]::Unicode.GetBytes((Clean-Secret $Password))
    $blob = [Runtime.InteropServices.Marshal]::AllocCoTaskMem($bytes.Length)

    try {
        [Runtime.InteropServices.Marshal]::Copy($bytes, 0, $blob, $bytes.Length)
        $credential = [CredentialManager.Credential]::new()
        $credential.Type = [CredentialManager.CredType]::Generic
        $credential.TargetName = $Target
        $credential.UserName = $UserName
        $credential.CredentialBlob = $blob
        $credential.CredentialBlobSize = $bytes.Length
        $credential.Persist = [CredentialManager.CredPersist]::LocalMachine

        $ok = [CredentialManager.NativeMethods]::CredWrite([ref]$credential, 0)
        if (-not $ok) {
            throw "CredWrite failed for $Target. Win32 error: $([Runtime.InteropServices.Marshal]::GetLastWin32Error())"
        }
    }
    finally {
        [Runtime.InteropServices.Marshal]::FreeCoTaskMem($blob)
    }
}

function Read-SecretText {
    param([Parameter(Mandatory)][string]$Prompt)

    $secure = Read-Host $Prompt -AsSecureString
    $bstr = [Runtime.InteropServices.Marshal]::SecureStringToBSTR($secure)
    try {
        [Runtime.InteropServices.Marshal]::PtrToStringBSTR($bstr)
    }
    finally {
        [Runtime.InteropServices.Marshal]::ZeroFreeBSTR($bstr)
    }
}

function ConvertFrom-EncryptedCredentialJson {
    param(
        [Parameter(Mandatory)][pscustomobject]$Envelope,
        [Parameter(Mandatory)][string]$Passphrase
    )

    if ($Envelope.Algorithm -ne "AES-256-GCM") {
        throw "Unsupported algorithm: $($Envelope.Algorithm)"
    }
    if ($Envelope.Kdf -ne "PBKDF2-HMAC-SHA256") {
        throw "Unsupported KDF: $($Envelope.Kdf)"
    }

    $salt = [Convert]::FromBase64String($Envelope.Salt)
    $nonce = [Convert]::FromBase64String($Envelope.Nonce)
    $tag = [Convert]::FromBase64String($Envelope.Tag)
    $ciphertext = [Convert]::FromBase64String($Envelope.Ciphertext)
    $key = [Security.Cryptography.Rfc2898DeriveBytes]::Pbkdf2(
        $Passphrase,
        $salt,
        [int]$Envelope.Iterations,
        [Security.Cryptography.HashAlgorithmName]::SHA256,
        32)
    $plaintext = [byte[]]::new($ciphertext.Length)

    try {
        try {
            $aes = [Security.Cryptography.AesGcm]::new($key, 16)
        }
        catch {
            $aes = [Security.Cryptography.AesGcm]::new($key)
        }

        try {
            $aes.Decrypt($nonce, $ciphertext, $tag, $plaintext)
            [Text.Encoding]::UTF8.GetString($plaintext)
        }
        finally {
            $aes.Dispose()
        }
    }
    finally {
        [Security.Cryptography.CryptographicOperations]::ZeroMemory($key)
    }
}

function Get-ShortDropboxName {
    param([Parameter(Mandatory)][string]$Name)

    if ($Name.StartsWith("dropbox_", [System.StringComparison]::OrdinalIgnoreCase)) {
        return $Name.Substring("dropbox_".Length)
    }

    $Name
}

function Get-TargetCredentialName {
    param(
        [Parameter(Mandatory)][string]$Prefix,
        [Parameter(Mandatory)][string]$Name
    )

    if ($Prefix -ieq "zen/default") {
        return "$Prefix/dropbox/$(Get-ShortDropboxName $Name)"
    }

    "$Prefix/$Name"
}

function Get-ImportedCredentialValue {
    param(
        [Parameter(Mandatory)][hashtable]$Credentials,
        [Parameter(Mandatory)][string]$ExportPrefix,
        [Parameter(Mandatory)][string]$Name
    )

    $shortName = Get-ShortDropboxName $Name
    $candidateKeys = @(
        (Get-TargetCredentialName "zen/default" $Name),
        "$ExportPrefix/dropbox/$shortName",
        "$ExportPrefix/dropbox/$Name",
        "$ExportPrefix/$Name",
        "$Name",
        "$shortName"
    ) | Where-Object { -not [string]::IsNullOrWhiteSpace($_) } | Select-Object -Unique

    foreach ($key in $candidateKeys) {
        $value = if ($Credentials.ContainsKey($key)) { Clean-Secret $Credentials[$key] } else { "" }
        if (-not [string]::IsNullOrWhiteSpace($value)) {
            return [pscustomobject]@{
                Key = $key
                Value = $value
            }
        }
    }

    $suffixMatches = $Credentials.Keys |
        Where-Object { $_ -match "([/\\:]|^)(dropbox_)?$([regex]::Escape($shortName))$" } |
        Sort-Object

    foreach ($key in $suffixMatches) {
        $value = Clean-Secret $Credentials[$key]
        if (-not [string]::IsNullOrWhiteSpace($value)) {
            return [pscustomobject]@{
                Key = $key
                Value = $value
            }
        }
    }

    $null
}

function Get-SecretFingerprint {
    param([string]$Value)

    if ([string]::IsNullOrWhiteSpace($Value)) {
        return "missing"
    }

    $bytes = [Text.Encoding]::UTF8.GetBytes((Clean-Secret $Value))
    $hash = [Security.Cryptography.SHA256]::HashData($bytes)
    ([Convert]::ToHexString($hash)).Substring(0, 12).ToLowerInvariant()
}

if (-not (Test-Path -LiteralPath $Path)) {
    throw "Encrypted credential JSON not found: $Path"
}

if ([string]::IsNullOrWhiteSpace($Passphrase)) {
    $Passphrase = [Environment]::GetEnvironmentVariable($PassphraseEnvVar)
}

if ([string]::IsNullOrWhiteSpace($Passphrase)) {
    $Passphrase = Read-SecretText "Encrypted JSON passphrase"
}

$envelope = Get-Content -LiteralPath $Path -Raw | ConvertFrom-Json
$plainJson = ConvertFrom-EncryptedCredentialJson -Envelope $envelope -Passphrase $Passphrase
$export = $plainJson | ConvertFrom-Json

$effectiveUserName = if (-not [string]::IsNullOrWhiteSpace($UserName)) {
    $UserName
}
elseif ($TargetPrefix -ieq "zen/default") {
    "zen"
}
elseif (-not [string]::IsNullOrWhiteSpace($export.UserName)) {
    $export.UserName
}
else {
    "dropbox"
}

$credentials = @{}
$export.Credentials.PSObject.Properties | ForEach-Object {
    $credentials[$_.Name] = Clean-Secret ([string]$_.Value)
}

Write-Host "Encrypted credential file decrypted."
Write-Host "Export prefix: $($export.Prefix)"
Write-Host "Target prefix: $TargetPrefix"
Write-Host "Credential user name: $effectiveUserName"
Write-Host "Credential targets:"

if ($PreserveTargets) {
    foreach ($target in ($credentials.Keys | Sort-Object)) {
        $value = Clean-Secret $credentials[$target]
        if ([string]::IsNullOrWhiteSpace($value)) {
            Write-Host "- ${target}: empty, skipped"
            continue
        }

        Write-Host "- ${target}: present"
        if (-not $TestOnly) {
            Set-WindowsCredentialPassword $target $effectiveUserName $value
        }
    }
}
else {
    foreach ($name in @("dropbox_refresh_token", "dropbox_app_key", "dropbox_app_secret")) {
        $source = Get-ImportedCredentialValue $credentials $export.Prefix $name
        $target = Get-TargetCredentialName $TargetPrefix $name
        if (-not $source) {
            Write-Host "- ${target}: missing in export"
            continue
        }

        $sourceValue = Clean-Secret $source.Value
        $storedValue = Clean-Secret (Get-WindowsCredentialPassword $target)
        $storedFingerprint = Get-SecretFingerprint $storedValue
        $sourceFingerprint = Get-SecretFingerprint $sourceValue
        $storedLength = if ([string]::IsNullOrWhiteSpace($storedValue)) { 0 } else { $storedValue.Length }
        $comparison = if ($storedFingerprint -eq $sourceFingerprint) { "matches current Credential Manager value" } else { "differs from current Credential Manager value" }

        Write-Host "- ${target}: present, from $($source.Key), length $($sourceValue.Length), sha256 $sourceFingerprint, $comparison (current length $storedLength, sha256 $storedFingerprint)"
        if (-not $TestOnly) {
            if (-not $CompareOnly) {
                Set-WindowsCredentialPassword $target $effectiveUserName $sourceValue
            }
        }
    }
}

if ($CompareOnly) {
    Write-Host "CompareOnly set; no Windows Credential Manager entries were written."
}
elseif ($TestOnly) {
    Write-Host "TestOnly set; no Windows Credential Manager entries were written."
}
else {
    Write-Host "Credentials imported into Windows Credential Manager."
}
