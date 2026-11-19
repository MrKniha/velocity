#requires -Version 5.1
<#
Where a process's memory is, roughly as VMMap shows it: committed bytes by region type (private
heap and stacks, mapped files, images), the image (DLL/EXE) data sections by module, and resident
private pages (working set) by the same split. For finding what to fix, not for the headline numbers
(those come from bench\memory.ps1).
Usage: powershell -File bench\vmmap.ps1 -Id <pid> [-Top 12]
#>
param([Parameter(Mandatory)][int]$Id, [int]$Top = 12)
$ErrorActionPreference = 'Stop'
Add-Type @'
using System; using System.Collections.Generic; using System.Runtime.InteropServices;
public static class VM {
  [StructLayout(LayoutKind.Sequential)] public struct MBI { public IntPtr Base, AllocBase; public uint AllocProtect; public IntPtr Size; public uint State, Protect, Type; }
  [StructLayout(LayoutKind.Sequential)] public struct WSEX { public IntPtr Va; public IntPtr Attr; }
  [DllImport("kernel32.dll")] public static extern IntPtr OpenProcess(int a, bool i, int id);
  [DllImport("kernel32.dll")] public static extern IntPtr VirtualQueryEx(IntPtr h, IntPtr a, out MBI m, IntPtr l);
  [DllImport("psapi.dll")] public static extern bool QueryWorkingSetEx(IntPtr h, [In, Out] WSEX[] b, int cb);
  [DllImport("psapi.dll", CharSet=CharSet.Unicode)] public static extern int GetMappedFileNameW(IntPtr h, IntPtr a, System.Text.StringBuilder s, int n);
  [DllImport("kernel32.dll")] public static extern bool CloseHandle(IntPtr h);
  // kind -> [committed, resident private]
  public static Dictionary<string, long[]> Walk(int pid, Dictionary<string, long[]> mods) {
    var r = new Dictionary<string, long[]>();
    IntPtr h = OpenProcess(0x0410 | 0x0008, false, pid);
    if (h == IntPtr.Zero) throw new Exception("OpenProcess failed");
    long a = 0; MBI m;
    var names = new Dictionary<long, string>();
    while (VirtualQueryEx(h, (IntPtr)a, out m, (IntPtr)Marshal.SizeOf(typeof(MBI))) != IntPtr.Zero) {
      long size = m.Size.ToInt64();
      if (m.State == 0x1000) { // MEM_COMMIT
        string kind = m.Type == 0x20000 ? "private" : m.Type == 0x40000 ? "mapped" : "image";
        long pages = size / 4096; long resPriv = 0;
        var buf = new WSEX[pages];
        for (long i = 0; i < pages; i++) buf[i].Va = (IntPtr)(a + i * 4096);
        if (QueryWorkingSetEx(h, buf, (int)(pages * Marshal.SizeOf(typeof(WSEX))))) {
          foreach (var e in buf) { long at = e.Attr.ToInt64(); if ((at & 1) != 0 && (at & (1L << 15)) == 0) resPriv += 4096; } // valid && !shared
        }
        if (!r.ContainsKey(kind)) r[kind] = new long[2];
        r[kind][0] += size; r[kind][1] += resPriv;
        if (m.Type == 0x1000000) {
          long ab = m.AllocBase.ToInt64(); string n;
          if (!names.TryGetValue(ab, out n)) { var sb = new System.Text.StringBuilder(512); GetMappedFileNameW(h, m.AllocBase, sb, 512); n = System.IO.Path.GetFileName(sb.ToString()); names[ab] = n; }
          if (!mods.ContainsKey(n)) mods[n] = new long[2];
          // writable data sections only (copy-on-write pages that became private count as resident private)
          bool writable = (m.Protect & (0x04 | 0x08 | 0x40 | 0x80)) != 0;
          if (writable) mods[n][0] += size;
          mods[n][1] += resPriv;
        }
      }
      a += size;
      if (a <= 0) break;
    }
    CloseHandle(h);
    return r;
  }
}
'@
$mods = New-Object 'System.Collections.Generic.Dictionary[string,long[]]'
$r = [VM]::Walk($Id, $mods)
"pid $Id  (MiB)        committed  resident-private"
foreach ($k in $r.Keys) { "{0,-18} {1,9:N1} {2,12:N1}" -f $k, ($r[$k][0] / 1MB), ($r[$k][1] / 1MB) }
"-- images by resident private (writable committed)"
$mods.GetEnumerator() | Sort-Object { $_.Value[1] } -Descending | Select-Object -First $Top | ForEach-Object { "{0,-28} {1,8:N1} ({2:N1})" -f $_.Key, ($_.Value[1] / 1MB), ($_.Value[0] / 1MB) }
