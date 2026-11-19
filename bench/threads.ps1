#requires -Version 5.1
<#
A process's threads grouped by their name (SetThreadDescription) and the module their start address
is in, for finding who starts too many threads.
Usage: powershell -File bench\threads.ps1 -Id <pid> [-Top 20]
#>
param([Parameter(Mandatory)][int]$Id, [int]$Top = 20)
$ErrorActionPreference = 'Stop'
Add-Type @'
using System; using System.Runtime.InteropServices;
public static class TH {
  [DllImport("kernel32.dll")] public static extern IntPtr OpenThread(int a, bool i, int id);
  [DllImport("kernel32.dll")] public static extern bool CloseHandle(IntPtr h);
  [DllImport("kernel32.dll", CharSet=CharSet.Unicode)] public static extern int GetThreadDescription(IntPtr h, out IntPtr d);
  [DllImport("kernel32.dll")] public static extern IntPtr LocalFree(IntPtr p);
  [DllImport("ntdll.dll")] public static extern int NtQueryInformationThread(IntPtr h, int c, out IntPtr info, int len, IntPtr ret);
  public static string Name(int tid) {
    IntPtr h = OpenThread(0x0800 | 0x0040, false, tid); if (h == IntPtr.Zero) return "?";
    IntPtr d; string s = "";
    if (GetThreadDescription(h, out d) >= 0 && d != IntPtr.Zero) { s = Marshal.PtrToStringUni(d); LocalFree(d); }
    CloseHandle(h); return s;
  }
  public static long Start(int tid) {
    IntPtr h = OpenThread(0x0040, false, tid); if (h == IntPtr.Zero) return 0;
    IntPtr a; int r = NtQueryInformationThread(h, 9, out a, IntPtr.Size, IntPtr.Zero); CloseHandle(h);
    return r == 0 ? a.ToInt64() : 0;
  }
}
'@
$p = Get-Process -Id $Id
$mods = @($p.Modules | ForEach-Object { [pscustomobject]@{ name = $_.ModuleName; lo = $_.BaseAddress.ToInt64(); hi = $_.BaseAddress.ToInt64() + $_.ModuleMemorySize } })
$rows = foreach ($t in $p.Threads) {
    $a = [TH]::Start($t.Id)
    $m = ($mods | Where-Object { $a -ge $_.lo -and $a -lt $_.hi } | Select-Object -First 1).name
    $n = [TH]::Name($t.Id) -replace '\d+$', '#'
    [pscustomobject]@{ key = "{0,-28} {1}" -f $n, $m }
}
"pid $Id ($($p.ProcessName)): $($p.Threads.Count) threads"
$rows | Group-Object key | Sort-Object Count -Descending | Select-Object -First $Top | ForEach-Object { "{0,5}  {1}" -f $_.Count, $_.Name }
