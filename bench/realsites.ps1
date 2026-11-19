#requires -Version 5.1
<#
Real sites, one per launch: memory of the whole Velocity process tree (browser + every child) after
the page loaded, a screenshot of the page (VELOCITY_SCREENSHOT), and whether it crashed (process gone
before the screenshot, or a "Page crashed" title in the history). Then all sites as tabs of one window,
after loading and after -Idle seconds. Live sites change from run to run: compare builds in the same
session, interleaved (-Exe a.exe,b.exe), never against an old run.
Nothing is clicked or typed into the pages: no logins, no cookie banners answered.
Usage: powershell -File bench\realsites.ps1 -Exe <velocity.exe>[,<other.exe>] [-Runs 2] [-Out file.json] [-Shots dir]
#>
param(
    [Parameter(Mandatory)][string[]]$Exe,
    [int]$Runs = 2,
    [string[]]$Sites = @('https://www.youtube.com/', 'https://www.facebook.com/', 'https://www.edupage.org/', 'https://www.alza.sk/',
        'https://www.riotgames.com/', 'https://store.steampowered.com/', 'https://en.wikipedia.org/wiki/Web_browser', 'https://www.reddit.com/',
        'https://github.com/', 'https://www.twitch.tv/', 'https://www.sme.sk/', 'https://www.google.com/', 'https://vimeo.com/',
        'https://open.spotify.com/', 'https://soundcloud.com/'),
    [int]$Settle = 25,
    [int]$Idle = 60,
    [switch]$NoTabs,
    [string]$Out,
    [string]$Shots = (Join-Path $env:TEMP 'velocity-shots')
)
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot\desktop.ps1"
$Exe = $Exe -split ',' | Where-Object { $_ }
$Sites = $Sites -split ',' | Where-Object { $_ }
$ubo = "$env:LOCALAPPDATA\velocity\extensions\uBOLite"
New-Item -ItemType Directory -Force $Shots | Out-Null

function Tree([int]$root) {
    $all = Get-CimInstance Win32_Process -Property ProcessId, ParentProcessId, Name
    $ids = @{ $root = 'browser' }
    do { $a = $false; foreach ($q in $all) { if ($ids.ContainsKey([int]$q.ParentProcessId) -and -not $ids.ContainsKey([int]$q.ProcessId)) { $ids[[int]$q.ProcessId] = $q.Name; $a = $true } } } while ($a)
    $ids
}
function Sample([int]$root) {
    $t = Tree $root
    $perf = @(Get-CimInstance Win32_PerfRawData_PerfProc_Process -Property IDProcess, WorkingSet, WorkingSetPrivate, PrivateBytes | Where-Object { $t.ContainsKey([int]$_.IDProcess) -and $_.IDProcess -ne 0 })
    $browser = $perf | Where-Object { $_.IDProcess -eq $root }
    [pscustomobject]@{
        processes = $perf.Count
        webview2 = @($t.Values | Where-Object { $_ -like 'msedgewebview2*' }).Count
        wsMiB = [math]::Round(($perf | Measure-Object WorkingSet -Sum).Sum / 1MB, 1)
        privWsMiB = [math]::Round(($perf | Measure-Object WorkingSetPrivate -Sum).Sum / 1MB, 1)
        commitMiB = [math]::Round(($perf | Measure-Object PrivateBytes -Sum).Sum / 1MB, 1)
        browserPrivWsMiB = [math]::Round($browser.WorkingSetPrivate / 1MB, 1)
        maxChildPrivWsMiB = [math]::Round((($perf | Where-Object { $_.IDProcess -ne $root } | Measure-Object WorkingSetPrivate -Maximum).Maximum) / 1MB, 1)
    }
}
function Launch([string]$path, [string[]]$arguments, [hashtable]$envs) {
    $data = Join-Path $env:TEMP ("velocity-real-" + [guid]::NewGuid().ToString('N').Substring(0, 8))
    New-Item -ItemType Directory -Force "$data\velocity\extensions" | Out-Null
    if (Test-Path "$ubo\manifest.json") { Copy-Item -Recurse $ubo "$data\velocity\extensions\uBOLite" }
    $saved = @{}; $envs['LOCALAPPDATA'] = $data; $envs['VELOCITY_TEST_COMMANDS'] = "$data\commands.txt"
    foreach ($k in $envs.Keys) { $saved[$k] = [Environment]::GetEnvironmentVariable($k); [Environment]::SetEnvironmentVariable($k, $envs[$k]) }
    try { $p = Start-OnBenchDesktop $path $arguments } # off the user's screen (bench\desktop.ps1)
    finally { foreach ($k in $saved.Keys) { [Environment]::SetEnvironmentVariable($k, $saved[$k]) } }
    [pscustomobject]@{ proc = $p; data = $data }
}
function Stop-All($v) {
    foreach ($id in (Tree $v.proc.Id).Keys) { try { Stop-Process -Id $id -Force -ErrorAction Stop } catch {} }
    Start-Sleep -Seconds 2
}
function Titles($v) {
    @(Get-ChildItem $v.data -Recurse -Filter history.tsv -ErrorAction SilentlyContinue | ForEach-Object { Get-Content $_.FullName } | ForEach-Object { ($_ -split "`t")[2] })
}
function Command($v, [string]$cmd) {
    $file = "$($v.data)\commands.txt"
    for ($i = 0; $i -lt 50 -and (Test-Path $file); $i++) { Start-Sleep -Milliseconds 100 }
    Set-Content -Path $file -Value $cmd -Encoding ascii
}
function Name([string]$url) { ([uri]$url).Host -replace '^(www|store|en|open)\.', '' }

$results = [System.Collections.Generic.List[object]]::new()
for ($run = 1; $run -le $Runs; $run++) {
    foreach ($site in $Sites) {
        foreach ($bin in $Exe) {
            $build = Split-Path (Split-Path $bin -Parent) -Leaf
            $shot = Join-Path $Shots ("{0}-{1}-run{2}.png" -f $build, (Name $site), $run)
            Remove-Item $shot, "$shot.ui.png" -ErrorAction SilentlyContinue
            $v = Launch $bin @($site) @{ VELOCITY_SCREENSHOT = $shot; VELOCITY_SCREENSHOT_DELAY = "$Settle" }
            Start-Sleep -Seconds ($Settle - 3)
            $s = if ($v.proc.HasExited) { $null } else { Sample $v.proc.Id }
            for ($i = 0; $i -lt 30 -and -not $v.proc.HasExited; $i++) { Start-Sleep -Milliseconds 500 }
            $titles = Titles $v
            $row = [pscustomobject]@{ scenario = 'site'; site = (Name $site); build = $build; run = $run
                alive = [bool]$s; shot = (Test-Path $shot); crashed = [bool]($titles -match 'Page crashed'); title = ($titles | Select-Object -Last 1)
                processes = $s.processes; webview2 = $s.webview2; wsMiB = $s.wsMiB; privWsMiB = $s.privWsMiB; commitMiB = $s.commitMiB
                browserPrivWsMiB = $s.browserPrivWsMiB; maxChildPrivWsMiB = $s.maxChildPrivWsMiB }
            $results.Add($row)
            "{0,-16} {1,-9} run{2} procs={3,-2} wv2={4} ws={5,7} privWs={6,7} commit={7,7} browser={8,6} maxChild={9,6} shot={10} crashed={11} title={12}" -f $row.site, $build, $run, $row.processes, $row.webview2, $row.wsMiB, $row.privWsMiB, $row.commitMiB, $row.browserPrivWsMiB, $row.maxChildPrivWsMiB, $row.shot, $row.crashed, $row.title | Write-Host
            Stop-All $v
            Remove-Item -Recurse -Force $v.data -ErrorAction SilentlyContinue
        }
    }
    if (-not $NoTabs) {
        foreach ($bin in $Exe) {
            $build = Split-Path (Split-Path $bin -Parent) -Leaf
            # Opening tabs needs the test mode (VELOCITY_TEST_COMMANDS); older builds get the single-site rows only.
            if (-not (Select-String -Path $bin -Pattern 'VELOCITY_TEST_COMMANDS' -SimpleMatch -Quiet)) { "tabs: $build has no test mode, skipped" | Write-Host; continue }
            $v = Launch $bin @($Sites[0]) @{}
            Start-Sleep -Seconds 15
            $n = 1
            foreach ($site in $Sites | Select-Object -Skip 1 -First 9) {
                Command $v 'key:new'; Start-Sleep -Milliseconds 1200; Command $v "go:$site"; Start-Sleep -Seconds 8; $n++
            }
            foreach ($point in "tabs$n", "tabs${n}idle$Idle") {
                if ($point -like '*idle*') { Start-Sleep -Seconds $Idle } else { Start-Sleep -Seconds $Settle }
                $s = if ($v.proc.HasExited) { $null } else { Sample $v.proc.Id }
                $titles = Titles $v
                $row = [pscustomobject]@{ scenario = 'tabs'; site = $point; build = $build; run = $run; alive = [bool]$s; shot = $false
                    crashed = @($titles -match 'Page crashed').Count; title = ''
                    processes = $s.processes; webview2 = $s.webview2; wsMiB = $s.wsMiB; privWsMiB = $s.privWsMiB; commitMiB = $s.commitMiB
                    browserPrivWsMiB = $s.browserPrivWsMiB; maxChildPrivWsMiB = $s.maxChildPrivWsMiB }
                $results.Add($row)
                "{0,-16} {1,-9} run{2} procs={3,-2} wv2={4} ws={5,7} privWs={6,7} commit={7,7} browser={8,6} maxChild={9,6} crashedTitles={10}" -f $point, $build, $run, $row.processes, $row.webview2, $row.wsMiB, $row.privWsMiB, $row.commitMiB, $row.browserPrivWsMiB, $row.maxChildPrivWsMiB, $row.crashed | Write-Host
            }
            Stop-All $v
            Remove-Item -Recurse -Force $v.data -ErrorAction SilentlyContinue
        }
    }
}
if ($Out) { $results | ConvertTo-Json -Depth 4 | Set-Content -Encoding utf8 $Out; "saved $Out" }


