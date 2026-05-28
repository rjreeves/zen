param(
    [Parameter (Mandatory = $false)]
    [string] $InstallDir = "C:\program files\zen"
)


$path = [Environment]::GetEnvironmentVariable("Path", "User")
$parts = $path -split ";" | Where-Object { $_ }

if ($parts -notcontains $InstallDir) {
    $newPath = ($parts + $InstallDir) -join ";"
    [Environment]::SetEnvironmentVariable("Path", $newPath, "User")
}