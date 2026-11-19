#requires -Version 5.1
<#
Speed: a JS/DOM workload on a local page (ms per part), and real sites' load times from Navigation
Timing (responseEnd, DOMContentLoaded, load, first contentful paint; ms from navigation start).
Velocity runs on the hidden bench desktop (bench\desktop.ps1) and is driven by its test mode, so the
build needs VELOCITY_TEST_COMMANDS and eval: support. Interleave builds in one run: live sites change.
Usage: powershell -File bench\speed.ps1 -Exe a.exe[,b.exe] [-Runs 3] [-Out file.json]
#>
param(
    [Parameter(Mandatory)][string[]]$Exe,
    [int]$Runs = 3,
    [string[]]$Sites = @('https://en.wikipedia.org/wiki/Web_browser', 'https://www.reddit.com/', 'https://www.alza.sk/', 'https://github.com/', 'https://www.youtube.com/', 'https://www.sme.sk/'),
    [int]$Wait = 20,
    [string]$Out
)
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot\desktop.ps1"
$Exe = $Exe -split ',' | Where-Object { $_ }
$Sites = $Sites -split ',' | Where-Object { $_ }

$work = @'
<!doctype html><title>work</title><div id=root></div><script>
const t = {}; const time = (k, f) => { const s = performance.now(); f(); t[k] = Math.round(performance.now() - s); };
time('dom', () => { const r = document.getElementById('root'); for (let i = 0; i < 20000; i++) { const d = document.createElement('div'); d.className = 'c' + (i % 7); d.textContent = 'item ' + i; r.appendChild(d); } });
time('layout', () => { const r = document.getElementById('root'); let h = 0; for (let i = 0; i < 50; i++) { r.style.width = (300 + i) + 'px'; h += r.offsetHeight; } });
time('json', () => { const o = []; for (let i = 0; i < 100000; i++) o.push({ id: i, name: 'n' + i, v: [i, i * 2, i * 3] }); const s = JSON.stringify(o); JSON.parse(s); });
time('regex', () => { const s = 'velocity browser engine '.repeat(20000); let n = 0; for (let i = 0; i < 20; i++) n += (s.match(/b\w+r/g) || []).length; });
time('sort', () => { const a = []; for (let i = 0; i < 300000; i++) a.push((i * 2654435761) % 1000003); a.sort((x, y) => x - y); });
time('alloc', () => { let keep = []; for (let i = 0; i < 2000000; i++) { keep.push({ a: i }); if (keep.length > 1000) keep = []; } });
window.__t = JSON.stringify(t);
</script>
'@

function Start-Velocity([string]$bin, [string]$url) {
    $data = Join-Path $env:TEMP ("velocity-speed-" + [guid]::NewGuid().ToString('N').Substring(0, 8))
    New-Item -ItemType Directory -Force "$data\velocity\extensions" | Out-Null
    $ubo = "$env:LOCALAPPDATA\velocity\extensions\uBOLite"; if (Test-Path "$ubo\manifest.json") { Copy-Item -Recurse $ubo "$data\velocity\extensions\uBOLite" }
    $old = $env:LOCALAPPDATA; $env:LOCALAPPDATA = $data; $env:VELOCITY_TEST_COMMANDS = "$data\commands.txt"
    try { $p = Start-OnBenchDesktop $bin @($url) } finally { $env:LOCALAPPDATA = $old; Remove-Item Env:VELOCITY_TEST_COMMANDS }
    [pscustomobject]@{ proc = $p; data = $data }
}
function Eval($v, [string]$js) {
    $out = "$($v.data)\eval-$([guid]::NewGuid().ToString('N').Substring(0,6)).txt"
    [IO.File]::WriteAllText("$($v.data)\commands.txt", "eval:$out|$js", [Text.UTF8Encoding]::new($false))
    for ($i = 0; $i -lt 60 -and -not (Test-Path $out); $i++) { Start-Sleep -Milliseconds 250 }
    if (Test-Path $out) { $r = Get-Content $out -Raw; if ($r -match 'String\("(.*)"\)\)') { return $Matches[1] -replace '\\"', '"' } }
    $null
}
function Stop-Velocity($v) {
    $all = Get-CimInstance Win32_Process -Property ProcessId, ParentProcessId; $ids = @($v.proc.Id)
    do { $a = $false; foreach ($q in $all) { if ($ids -contains [int]$q.ParentProcessId -and -not ($ids -contains [int]$q.ProcessId)) { $ids += [int]$q.ProcessId; $a = $true } } } while ($a)
    foreach ($id in $ids) { try { Stop-Process -Id $id -Force -ErrorAction Stop } catch {} }
    Start-Sleep 2; Remove-Item -Recurse -Force $v.data -ErrorAction SilentlyContinue
}

$fx = Join-Path $env:TEMP ("velocity-speed-fx-" + [guid]::NewGuid().ToString('N').Substring(0, 6)); New-Item -ItemType Directory $fx | Out-Null
Set-Content "$fx\work.html" -Encoding utf8 -Value $work
$port = Get-Random -Minimum 20000 -Maximum 40000
$server = Start-Process python -ArgumentList '-I', '-m', 'http.server', "$port", '--bind', '127.0.0.1', '--directory', $fx -PassThru -WindowStyle Hidden
$timing = "(() => { const n = performance.getEntriesByType('navigation')[0] || {}; const p = performance.getEntriesByType('paint').find(e => e.name == 'first-contentful-paint'); return JSON.stringify({ response: Math.round(n.responseEnd || 0), dcl: Math.round(n.domContentLoadedEventEnd || 0), load: Math.round(n.loadEventEnd || 0), fcp: p ? Math.round(p.startTime) : null }); })()"
$results = [System.Collections.Generic.List[object]]::new()
try {
    Start-Sleep 1
    for ($run = 1; $run -le $Runs; $run++) {
        foreach ($bin in $Exe) {
            $build = Split-Path (Split-Path $bin -Parent) -Leaf
            $v = Start-Velocity $bin "http://127.0.0.1:$port/work.html"
            Start-Sleep 12
            $r = Eval $v 'window.__t'
            Stop-Velocity $v
            $results.Add([pscustomobject]@{ kind = 'work'; build = $build; run = $run; site = 'work'; result = $r })
            "work      {0,-9} run{1} {2}" -f $build, $run, $r | Write-Host
            foreach ($site in $Sites) {
                $v = Start-Velocity $bin $site
                Start-Sleep $Wait
                $r = Eval $v $timing
                Stop-Velocity $v
                $name = ([uri]$site).Host -replace '^(www|en)\.', ''
                $results.Add([pscustomobject]@{ kind = 'site'; build = $build; run = $run; site = $name; result = $r })
                "{0,-9} {1,-9} run{2} {3}" -f $name, $build, $run, $r | Write-Host
            }
        }
    }
} finally {
    Stop-Process -Id $server.Id -Force -ErrorAction SilentlyContinue
    Remove-Item -Recurse -Force $fx -ErrorAction SilentlyContinue
}
if ($Out) { $results | ConvertTo-Json -Depth 4 | Set-Content -Encoding utf8 $Out; "saved $Out" }
