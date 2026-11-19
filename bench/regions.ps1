#requires -Version 5.1
<#
A process's committed private memory by allocation (VirtualAlloc reservation), largest first, with how
much of each is resident: frame-sized buffers (a video's), heaps and stacks stand out by their sizes.
Usage: powershell -File bench\regions.ps1 -Id <pid> [-Top 25]
#>
param([Parameter(Mandatory)][int]$Id, [int]$Top = 25)
$ErrorActionPreference = 'Stop'
Add-Type @'
using System; using System.Collections.Generic; using System.Runtime.InteropServices;
public static class RG {
  [StructLayout(LayoutKind.Sequential)] public struct MBI { public IntPtr Base, AllocBase; public uint AllocProtect; public IntPtr Size; public uint State, Protect, Type; }
  [StructLayout(LayoutKind.Sequential)] public struct WSEX { public IntPtr Va; public IntPtr Attr; }
  [DllImport("kernel32.dll")] public static extern IntPtr OpenProcess(int a, bool i, int id);
  [DllImport("kernel32.dll")] public static extern IntPtr VirtualQueryEx(IntPtr h, IntPtr a, out MBI m, IntPtr l);
  [DllImport("psapi.dll")] public static extern bool QueryWorkingSetEx(IntPtr h, [In, Out] WSEX[] b, int cb);
  [DllImport("kernel32.dll")] public static extern bool CloseHandle(IntPtr h);
  // allocation base -> [committed, resident]
  public static Dictionary<long, long[]> Walk(int pid) {
    var r = new Dictionary<long, long[]>();
    IntPtr h = OpenProcess(0x0410, false, pid); if (h == IntPtr.Zero) throw new Exception("OpenProcess failed");
    long a = 0; MBI m;
    while (VirtualQueryEx(h, (IntPtr)a, out m, (IntPtr)Marshal.SizeOf(typeof(MBI))) != IntPtr.Zero) {
      long size = m.Size.ToInt64();
      if (m.State == 0x1000 && m.Type == 0x20000) {
        long pages = size / 4096; long res = 0; var buf = new WSEX[pages];
        for (long i = 0; i < pages; i++) buf[i].Va = (IntPtr)(a + i * 4096);
        if (QueryWorkingSetEx(h, buf, (int)(pages * Marshal.SizeOf(typeof(WSEX))))) foreach (var e in buf) if ((e.Attr.ToInt64() & 1) != 0) res += 4096;
        long ab = m.AllocBase.ToInt64(); if (!r.ContainsKey(ab)) r[ab] = new long[2];
        r[ab][0] += size; r[ab][1] += res;
      }
      a += size; if (a <= 0) break;
    }
    CloseHandle(h); return r;
  }
}
'@
$r = [RG]::Walk($Id)
$all = $r.GetEnumerator() | ForEach-Object { [pscustomobject]@{ base = $_.Key; mib = $_.Value[0] / 1MB; res = $_.Value[1] / 1MB } }
"pid {0}: {1} private allocations, {2:N1} MiB committed, {3:N1} resident" -f $Id, $all.Count, ($all | Measure-Object mib -Sum).Sum, ($all | Measure-Object res -Sum).Sum
"-- by size (MiB, rounded): count x size = resident"
$all | Group-Object { [math]::Round($_.mib, 1) } | ForEach-Object { [pscustomobject]@{ size = [double]$_.Name; n = $_.Count; res = ($_.Group | Measure-Object res -Sum).Sum } } |
    Sort-Object res -Descending | Select-Object -First $Top | ForEach-Object { "{0,4} x {1,7:N1} = {2,7:N1}" -f $_.n, $_.size, $_.res }
