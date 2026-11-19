#requires -Version 5.1
<#
Velocity memory scenarios. Every number is summed over velocity.exe and ALL its descendant processes
(content processes, and WebView2 processes if a build still starts any), sampled once a second.

Per sample, summed over the process tree:
  wsMiB       working set (private + shared pages resident in RAM; what "Memory" columns often show)
  privWsMiB   private working set (Task Manager's "Memory (private working set)")
  commitMiB   private bytes / commit charge (cannot be trimmed by the OS)
Peak = the largest tree sum seen by the 1 s sampler during the scenario. GPU dedicated memory is
reported separately (GPU Process Memory counters) and is not part of the RAM numbers.
1 MiB = 1048576 bytes; MB (10^6) values are derived as MiB * 1.048576.

Scenarios (each in a fresh Velocity data folder, %LOCALAPPDATA% redirected; uBlock Origin Lite seeded):
  home      start with the new tab page (home page); after load (10 s) and after 60 s idle
  tabs      first fixture page on the command line, then a new tab + address for pages 2..10;
            measured at 1, 5 and 10 tabs after load, and 10 tabs after 60 s idle
  churn     20 cycles of open tab -> load page -> close tab; measured every 5 cycles
  navigate  30 navigations in one tab across the 10 pages (Ctrl+L, address, Enter); every 10

Velocity is driven through its test mode (VELOCITY_TEST_COMMANDS: tab UI commands from a file), never
with keys sent to the focused window. Builds without that test mode are refused unless -SendKeys is given
(the original baseline was measured that way: then it only types while Velocity has the focus).
Usage: powershell -File bench\memory.ps1 -Exe <velocity.exe> [-Runs 3] [-Scenarios home,tabs,churn,navigate] [-Out file.json]
#>
param(
    [Parameter(Mandatory)][string]$Exe,
    [int]$Runs = 3,
    [string[]]$Scenarios = @('home', 'tabs', 'churn', 'navigate'),
    [string]$Out,
    [int]$Port = 8765,
    [int]$Settle = 10,
    [int]$Idle = 60,
    [switch]$SendKeys
)
$ErrorActionPreference = 'Stop'
$Scenarios = $Scenarios -split ',' | Where-Object { $_ }
$root = Split-Path $PSScriptRoot -Parent
$fixtures = Join-Path $PSScriptRoot 'fixtures'
if (-not (Test-Path "$fixtures\page10.html")) { python -I "$PSScriptRoot\make_fixtures.py" | Out-Null }
$ubo = "$env:LOCALAPPDATA\velocity\extensions\uBOLite"
Add-Type -AssemblyName System.Windows.Forms, Microsoft.VisualBasic
Add-Type @'
using System; using System.Runtime.InteropServices;
public static class Win {
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
  [DllImport("user32.dll")] public static extern int GetWindowThreadProcessId(IntPtr h, out int pid);
}
'@

function Get-Tree([int]$rootPid) {
    $all = Get-CimInstance Win32_Process -Property ProcessId, ParentProcessId, Name
    $ids = [System.Collections.Generic.HashSet[int]]::new(); [void]$ids.Add($rootPid)
    do {
        $added = $false
        foreach ($p in $all) { if ($ids.Contains([int]$p.ParentProcessId) -and $ids.Add([int]$p.ProcessId)) { $added = $true } }
    } while ($added)
    $names = @{}; foreach ($p in $all) { if ($ids.Contains([int]$p.ProcessId)) { $names[[int]$p.ProcessId] = $p.Name } }
    $names
}

function Sample([int]$rootPid) {
    $tree = Get-Tree $rootPid
    $perf = @(Get-CimInstance Win32_PerfRawData_PerfProc_Process -Property IDProcess, WorkingSet, WorkingSetPrivate, PrivateBytes |
        Where-Object { $tree.ContainsKey([int]$_.IDProcess) -and $_.IDProcess -ne 0 })
    [pscustomobject]@{
        processes = $perf.Count
        webview2 = @($tree.Values | Where-Object { $_ -like 'msedgewebview2*' }).Count
        wsMiB = [math]::Round(($perf | Measure-Object WorkingSet -Sum).Sum / 1MB, 1)
        privWsMiB = [math]::Round(($perf | Measure-Object WorkingSetPrivate -Sum).Sum / 1MB, 1)
        commitMiB = [math]::Round(($perf | Measure-Object PrivateBytes -Sum).Sum / 1MB, 1)
        pids = @($tree.Keys)
    }
}

function Gpu-MiB($pids) {
    try {
        $c = (Get-Counter '\GPU Process Memory(*)\Dedicated Usage' -ErrorAction Stop).CounterSamples
        $sum = 0; foreach ($s in $c) { if ($s.InstanceName -match '^pid_(\d+)_' -and $pids -contains [int]$Matches[1]) { $sum += $s.CookedValue } }
        [math]::Round($sum / 1MB, 1)
    } catch { $null }
}

# Background sampler: the largest tree sum while a scenario runs.
$script:peak = $null
function Peak-Start([int]$rootPid) {
    $script:peakJob = Start-Job -ArgumentList $rootPid -ScriptBlock {
        param($rootPid)
        $max = [pscustomobject]@{ wsMiB = 0; privWsMiB = 0; commitMiB = 0 }
        while ($true) {
            $all = Get-CimInstance Win32_Process -Property ProcessId, ParentProcessId
            $ids = [System.Collections.Generic.HashSet[int]]::new(); [void]$ids.Add($rootPid)
            do { $a = $false; foreach ($p in $all) { if ($ids.Contains([int]$p.ParentProcessId) -and $ids.Add([int]$p.ProcessId)) { $a = $true } } } while ($a)
            if (-not ($all | Where-Object ProcessId -eq $rootPid)) { break }
            $perf = @(Get-CimInstance Win32_PerfRawData_PerfProc_Process -Property IDProcess, WorkingSet, WorkingSetPrivate, PrivateBytes | Where-Object { $ids.Contains([int]$_.IDProcess) -and $_.IDProcess -ne 0 })
            $ws = ($perf | Measure-Object WorkingSet -Sum).Sum / 1MB; $pw = ($perf | Measure-Object WorkingSetPrivate -Sum).Sum / 1MB; $cm = ($perf | Measure-Object PrivateBytes -Sum).Sum / 1MB
            if ($ws -gt $max.wsMiB) { $max.wsMiB = [math]::Round($ws, 1) }
            if ($pw -gt $max.privWsMiB) { $max.privWsMiB = [math]::Round($pw, 1) }
            if ($cm -gt $max.commitMiB) { $max.commitMiB = [math]::Round($cm, 1) }
            Set-Content -Path "$env:TEMP\velocity-peak-$rootPid.json" -Value ($max | ConvertTo-Json -Compress)
            Start-Sleep -Milliseconds 700
        }
    }
}
function Peak-Read([int]$rootPid) {
    try { Get-Content "$env:TEMP\velocity-peak-$rootPid.json" -Raw | ConvertFrom-Json } catch { $null }
}

function Launch([string[]]$arguments) {
    $data = Join-Path $env:TEMP ("velocity-mem-" + [guid]::NewGuid().ToString('N').Substring(0, 8))
    New-Item -ItemType Directory -Force "$data\velocity\extensions" | Out-Null
    if (Test-Path "$ubo\manifest.json") { Copy-Item -Recurse $ubo "$data\velocity\extensions\uBOLite" }
    $old = $env:LOCALAPPDATA; $env:LOCALAPPDATA = $data; $env:VELOCITY_TEST_COMMANDS = "$data\commands.txt"
    try {
        $p = if ($arguments) { Start-Process -FilePath $Exe -ArgumentList $arguments -PassThru } else { Start-Process -FilePath $Exe -PassThru }
    } finally { $env:LOCALAPPDATA = $old; Remove-Item Env:VELOCITY_TEST_COMMANDS }
    Start-Sleep -Seconds 3
    for ($i = 0; $i -lt 20 -and $p.MainWindowHandle -eq 0; $i++) { Start-Sleep -Milliseconds 500; $p.Refresh() }
    Peak-Start $p.Id
    [pscustomobject]@{ proc = $p; data = $data }
}

function Stop-Velocity($v) {
    Get-Job | Stop-Job -PassThru | Remove-Job
    $tree = Get-Tree $v.proc.Id
    foreach ($id in $tree.Keys) { try { Stop-Process -Id $id -Force -ErrorAction Stop } catch {} }
    Start-Sleep -Seconds 2
    Remove-Item -Recurse -Force $v.data -ErrorAction SilentlyContinue
    Remove-Item "$env:TEMP\velocity-peak-$($v.proc.Id).json" -ErrorAction SilentlyContinue
}

function Keys($v, [string]$keys) {
    $v.proc.Refresh()
    [void][Win]::SetForegroundWindow($v.proc.MainWindowHandle)
    try { [Microsoft.VisualBasic.Interaction]::AppActivate($v.proc.Id) } catch {}
    Start-Sleep -Milliseconds 300
    # Never type into another program: only while a Velocity window has the focus.
    $owner = 0; [void][Win]::GetWindowThreadProcessId([Win]::GetForegroundWindow(), [ref]$owner)
    if (-not (Get-Tree $v.proc.Id).ContainsKey($owner)) { throw "Velocity lost the focus (pid $owner has it): stopped, no keys sent" }
    [System.Windows.Forms.SendKeys]::SendWait($keys)
}

# One tab UI command through the test mode, or the same action as keys.
function Command($v, [string]$cmd) {
    if ($SendKeys) {
        switch -regex ($cmd) {
            '^key:new$' { Keys $v '^t' }
            '^key:close$' { Keys $v '^w' }
            '^go:(.*)$' { Keys $v '^l'; Start-Sleep -Milliseconds 600; Keys $v ($Matches[1] + '{ENTER}') }
        }
        return
    }
    $file = "$($v.data)\commands.txt"
    for ($i = 0; $i -lt 50 -and (Test-Path $file); $i++) { Start-Sleep -Milliseconds 100 }
    Set-Content -Path $file -Value $cmd -Encoding ascii
}

function Url([int]$n) { "http://127.0.0.1:$Port/page$n.html" }

function Open-Tab($v, [int]$n) {
    Command $v 'key:new'; Start-Sleep -Milliseconds 1200
    Command $v ('go:' + (Url $n))
}

function Record($results, [string]$scenario, [int]$run, [string]$point, $v) {
    $s = Sample $v.proc.Id
    $gpu = Gpu-MiB $s.pids
    $pk = Peak-Read $v.proc.Id
    $row = [pscustomobject]@{ scenario = $scenario; run = $run; point = $point; processes = $s.processes; webview2 = $s.webview2
        wsMiB = $s.wsMiB; privWsMiB = $s.privWsMiB; commitMiB = $s.commitMiB; gpuMiB = $gpu
        peakWsMiB = $pk.wsMiB; peakPrivWsMiB = $pk.privWsMiB; peakCommitMiB = $pk.commitMiB; alive = -not $v.proc.HasExited }
    $results.Add($row)
    "{0,-9} run{1} {2,-14} procs={3,-2} wv2={4} ws={5,7} privWs={6,7} commit={7,7} gpu={8} peakWs={9}" -f $scenario, $run, $point, $s.processes, $s.webview2, $s.wsMiB, $s.privWsMiB, $s.commitMiB, $gpu, $pk.wsMiB | Write-Host
}

# Loopback server for the fixtures.
$server = Start-Process python -ArgumentList '-I', '-m', 'http.server', $Port, '--bind', '127.0.0.1', '--directory', $fixtures -PassThru -WindowStyle Hidden
Start-Sleep -Seconds 1
$results = [System.Collections.Generic.List[object]]::new()
try {
    if (-not $SendKeys -and -not (Select-String -Path $Exe -Pattern 'VELOCITY_TEST_COMMANDS' -SimpleMatch -Quiet)) {
        throw "$Exe has no test mode (VELOCITY_TEST_COMMANDS); pass -SendKeys to drive it with keys"
    }
    # Warm-up (GStreamer registry, file cache); not recorded.
    $v = Launch @(); Start-Sleep -Seconds 15; Stop-Velocity $v
    for ($run = 1; $run -le $Runs; $run++) {
        if ($Scenarios -contains 'home') {
            $v = Launch @()
            Start-Sleep -Seconds $Settle; Record $results home $run 'loaded' $v
            Start-Sleep -Seconds $Idle; Record $results home $run 'idle60' $v
            Stop-Velocity $v
        }
        if ($Scenarios -contains 'tabs') {
            $v = Launch @((Url 1))
            Start-Sleep -Seconds $Settle; Record $results tabs $run 'tabs1' $v
            for ($n = 2; $n -le 10; $n++) {
                Open-Tab $v $n; Start-Sleep -Seconds 4
                if ($n -eq 5) { Start-Sleep -Seconds $Settle; Record $results tabs $run 'tabs5' $v }
            }
            Start-Sleep -Seconds $Settle; Record $results tabs $run 'tabs10' $v
            Start-Sleep -Seconds $Idle; Record $results tabs $run 'tabs10idle60' $v
            Stop-Velocity $v
        }
        if ($Scenarios -contains 'churn') {
            $v = Launch @((Url 1))
            Start-Sleep -Seconds $Settle; Record $results churn $run 'cycle0' $v
            for ($c = 1; $c -le 20; $c++) {
                Open-Tab $v (($c % 10) + 1); Start-Sleep -Seconds 4
                Command $v 'key:close'; Start-Sleep -Seconds 1
                if ($c % 5 -eq 0) { Start-Sleep -Seconds 5; Record $results churn $run "cycle$c" $v }
            }
            Stop-Velocity $v
        }
        if ($Scenarios -contains 'navigate') {
            $v = Launch @((Url 1))
            Start-Sleep -Seconds $Settle; Record $results navigate $run 'nav0' $v
            for ($k = 1; $k -le 30; $k++) {
                Command $v ('go:' + (Url (($k % 10) + 1))); Start-Sleep -Seconds 3
                if ($k % 10 -eq 0) { Start-Sleep -Seconds 5; Record $results navigate $run "nav$k" $v }
            }
            Stop-Velocity $v
        }
    }
} finally {
    Stop-Process -Id $server.Id -Force -ErrorAction SilentlyContinue
}
if ($Out) { $results | ConvertTo-Json -Depth 4 | Set-Content -Encoding utf8 $Out; "saved $Out" }
