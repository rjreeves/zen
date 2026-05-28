# system_info.ps1
# Read-only Windows system information report.
# Reports:
#   - OS / machine / CPU
#   - physical memory totals and usage
#   - fixed local disk size/free/used
#
# Optional:
#   powershell.exe -NoProfile -ExecutionPolicy Bypass -File examples/system_info.ps1 -JsonOut system_info_latest.json
#   powershell.exe -NoProfile -ExecutionPolicy Bypass -File examples/system_info.ps1 -JsonOnly

[CmdletBinding()]
param(
    [string]$JsonOut = "",
    [switch]$JsonOnly
)

$ErrorActionPreference = "Stop"

function Convert-KbToGb {
    param([double]$Kb)
    if ($null -eq $Kb) { return 0 }
    return [math]::Round(($Kb * 1KB) / 1GB, 2)
}

function Convert-BytesToGb {
    param([double]$Bytes)
    if ($null -eq $Bytes) { return 0 }
    return [math]::Round($Bytes / 1GB, 2)
}

function Safe-Percent {
    param(
        [double]$Part,
        [double]$Total
    )
    if ($Total -le 0) { return 0 }
    return [math]::Round(($Part / $Total) * 100, 1)
}

$now = Get-Date
$os = Get-CimInstance Win32_OperatingSystem
$computer = Get-CimInstance Win32_ComputerSystem
$cpu = Get-CimInstance Win32_Processor | Select-Object -First 1
$bios = Get-CimInstance Win32_BIOS

$totalMemoryGb = Convert-KbToGb $os.TotalVisibleMemorySize
$freeMemoryGb = Convert-KbToGb $os.FreePhysicalMemory
$usedMemoryGb = [math]::Round($totalMemoryGb - $freeMemoryGb, 2)
$usedMemoryPercent = Safe-Percent $usedMemoryGb $totalMemoryGb

$diskRows = Get-CimInstance Win32_LogicalDisk -Filter "DriveType=3" |
    Sort-Object DeviceID |
    ForEach-Object {
        $sizeGb = Convert-BytesToGb $_.Size
        $freeGb = Convert-BytesToGb $_.FreeSpace
        $usedGb = [math]::Round($sizeGb - $freeGb, 2)

        [PSCustomObject]@{
            Drive       = $_.DeviceID
            Label       = $_.VolumeName
            FileSystem  = $_.FileSystem
            SizeGB      = $sizeGb
            UsedGB      = $usedGb
            FreeGB      = $freeGb
            UsedPercent = Safe-Percent $usedGb $sizeGb
            FreePercent = Safe-Percent $freeGb $sizeGb
        }
    }

$report = [ordered]@{
    collected_at = $now.ToString("yyyy-MM-dd HH:mm:ss")
    machine = [ordered]@{
        computer_name = $env:COMPUTERNAME
        user_name = $env:USERNAME
        manufacturer = $computer.Manufacturer
        model = $computer.Model
        domain = $computer.Domain
    }
    os = [ordered]@{
        caption = $os.Caption
        version = $os.Version
        build_number = $os.BuildNumber
        architecture = $os.OSArchitecture
        last_boot = $os.LastBootUpTime
        uptime_days = [math]::Round(($now - $os.LastBootUpTime).TotalDays, 2)
    }
    cpu = [ordered]@{
        name = $cpu.Name
        physical_cores = $cpu.NumberOfCores
        logical_processors = $cpu.NumberOfLogicalProcessors
        max_clock_mhz = $cpu.MaxClockSpeed
    }
    bios = [ordered]@{
        manufacturer = $bios.Manufacturer
        version = $bios.SMBIOSBIOSVersion
        serial_number = $bios.SerialNumber
    }
    memory = [ordered]@{
        total_gb = $totalMemoryGb
        used_gb = $usedMemoryGb
        free_gb = $freeMemoryGb
        used_percent = $usedMemoryPercent
    }
    disks = @($diskRows)
}

$json = $report | ConvertTo-Json -Depth 8

if ($JsonOut -ne "") {
    $json | Set-Content -Path $JsonOut -Encoding UTF8
}

if ($JsonOnly) {
    $json
    exit 0
}

Write-Output "============================================================"
Write-Output "ZEN SYSTEM INFORMATION REPORT"
Write-Output "Collected: $($report.collected_at)"
Write-Output "============================================================"
Write-Output ""

Write-Output "SYSTEM"
Write-Output "------"
Write-Output ("Computer : {0}" -f $report.machine.computer_name)
Write-Output ("User     : {0}" -f $report.machine.user_name)
Write-Output ("Make     : {0}" -f $report.machine.manufacturer)
Write-Output ("Model    : {0}" -f $report.machine.model)
Write-Output ("Domain   : {0}" -f $report.machine.domain)
Write-Output ""

Write-Output "OPERATING SYSTEM"
Write-Output "----------------"
Write-Output ("OS       : {0}" -f $report.os.caption)
Write-Output ("Version  : {0}" -f $report.os.version)
Write-Output ("Build    : {0}" -f $report.os.build_number)
Write-Output ("Arch     : {0}" -f $report.os.architecture)
Write-Output ("LastBoot : {0}" -f $report.os.last_boot)
Write-Output ("Uptime   : {0} days" -f $report.os.uptime_days)
Write-Output ""

Write-Output "CPU"
Write-Output "---"
Write-Output ("Name     : {0}" -f $report.cpu.name)
Write-Output ("Cores    : {0} physical / {1} logical" -f $report.cpu.physical_cores, $report.cpu.logical_processors)
Write-Output ("Max MHz  : {0}" -f $report.cpu.max_clock_mhz)
Write-Output ""

Write-Output "MEMORY"
Write-Output "------"
Write-Output ("Total    : {0} GB" -f $report.memory.total_gb)
Write-Output ("Used     : {0} GB ({1}%)" -f $report.memory.used_gb, $report.memory.used_percent)
Write-Output ("Free     : {0} GB" -f $report.memory.free_gb)
Write-Output ""

Write-Output "FIXED DISKS"
Write-Output "-----------"
if ($diskRows.Count -eq 0) {
    Write-Output "No fixed local disks found."
} else {
    $diskRows |
        Format-Table Drive, Label, FileSystem, SizeGB, UsedGB, FreeGB, UsedPercent, FreePercent -AutoSize |
        Out-String -Width 200 |
        Write-Output
}

if ($JsonOut -ne "") {
    Write-Output ""
    Write-Output ("JSON written to: {0}" -f $JsonOut)
}
