param(
    [string[]] $Target,
    [string] $TargetFile
)

$ErrorActionPreference = "Stop"

Add-Type -TypeDefinition @"
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
using System.Text;

public static class CredentialWriter {
    private const int CRED_TYPE_GENERIC = 1;
    private const int CRED_PERSIST_LOCAL_MACHINE = 2;

    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    private struct CREDENTIAL {
        public UInt32 Flags;
        public UInt32 Type;
        public string TargetName;
        public string Comment;
        public System.Runtime.InteropServices.ComTypes.FILETIME LastWritten;
        public UInt32 CredentialBlobSize;
        public IntPtr CredentialBlob;
        public UInt32 Persist;
        public UInt32 AttributeCount;
        public IntPtr Attributes;
        public string TargetAlias;
        public string UserName;
    }

    [DllImport("advapi32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
    private static extern bool CredWriteW(ref CREDENTIAL credential, UInt32 flags);

    public static void WriteGeneric(string targetName, string userName, string secret) {
        byte[] secretBytes = Encoding.UTF8.GetBytes(secret);
        IntPtr blob = Marshal.AllocHGlobal(secretBytes.Length);
        try {
            Marshal.Copy(secretBytes, 0, blob, secretBytes.Length);
            var credential = new CREDENTIAL {
                Flags = 0,
                Type = CRED_TYPE_GENERIC,
                TargetName = targetName,
                Comment = null,
                CredentialBlobSize = (UInt32)secretBytes.Length,
                CredentialBlob = blob,
                Persist = CRED_PERSIST_LOCAL_MACHINE,
                AttributeCount = 0,
                Attributes = IntPtr.Zero,
                TargetAlias = null,
                UserName = userName
            };

            if (!CredWriteW(ref credential, 0)) {
                throw new Win32Exception(Marshal.GetLastWin32Error());
            }
        }
        finally {
            Array.Clear(secretBytes, 0, secretBytes.Length);
            Marshal.FreeHGlobal(blob);
        }
    }
}
"@

function ConvertFrom-SecureStringPlainText {
    param([securestring] $Secure)

    $bstr = [Runtime.InteropServices.Marshal]::SecureStringToBSTR($Secure)
    try {
        [Runtime.InteropServices.Marshal]::PtrToStringBSTR($bstr)
    }
    finally {
        [Runtime.InteropServices.Marshal]::ZeroFreeBSTR($bstr)
    }
}

$targets = @()

if ($TargetFile) {
    $targets += Get-Content -LiteralPath $TargetFile |
        ForEach-Object { $_.Trim() } |
        Where-Object { $_ -and -not $_.StartsWith("#") }
}

if ($Target) {
    $targets += $Target
}

while ($targets.Count -eq 0) {
    $name = Read-Host "Credential target name, e.g. fw.dbx.data... (blank to finish)"
    if ([string]::IsNullOrWhiteSpace($name)) {
        break
    }
    $targets += $name.Trim()
}

$targets = $targets | Select-Object -Unique
if ($targets.Count -eq 0) {
    throw "No credential targets supplied."
}

foreach ($name in $targets) {
    if ($name -notlike "fw.dbx.data*") {
        Write-Warning "Target '$name' does not start with 'fw.dbx.data'."
    }

    $secure = Read-Host "Secret value for '$name'" -AsSecureString
    $plain = ConvertFrom-SecureStringPlainText $secure
    try {
        [CredentialWriter]::WriteGeneric($name, "zen", $plain)
        Write-Host "Saved Credential Manager generic credential: $name"
    }
    finally {
        $plain = $null
    }
}
