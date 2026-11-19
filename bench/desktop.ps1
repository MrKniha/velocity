#requires -Version 5.1
<#
Dot-source this: `Start-OnBenchDesktop <exe> <arguments>` starts a program on a desktop of its own
("WinSta0\VelocityBench"), below normal priority, and returns its Process. Its windows (and its child
processes', which inherit the desktop) are not on the user's screen and never take the focus, so
benchmarks can run while the computer is in use. The program gets this process's environment.
#>
if (-not ('BenchDesktop' -as [type])) {
    Add-Type @'
using System; using System.ComponentModel; using System.Runtime.InteropServices; using System.Text;
public static class BenchDesktop {
  [DllImport("user32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
  static extern IntPtr CreateDesktop(string name, IntPtr device, IntPtr devmode, int flags, uint access, IntPtr sa);
  [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
  struct STARTUPINFO { public int cb; public string lpReserved, lpDesktop, lpTitle; public int dwX, dwY, dwXSize, dwYSize, dwXCountChars, dwYCountChars, dwFillAttribute, dwFlags; public short wShowWindow, cbReserved2; public IntPtr lpReserved2, hStdInput, hStdOutput, hStdError; }
  [StructLayout(LayoutKind.Sequential)]
  struct PROCESS_INFORMATION { public IntPtr hProcess, hThread; public int pid, tid; }
  [DllImport("kernel32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
  static extern bool CreateProcess(string app, StringBuilder cmd, IntPtr pa, IntPtr ta, bool inherit, uint flags, IntPtr env, string dir, ref STARTUPINFO si, out PROCESS_INFORMATION pi);
  [DllImport("kernel32.dll")] static extern bool CloseHandle(IntPtr h);
  static IntPtr desktop; // kept open: the desktop lives as long as a handle or a thread is on it
  public static int Start(string exe, string arguments) {
    if (desktop == IntPtr.Zero) {
      desktop = CreateDesktop("VelocityBench", IntPtr.Zero, IntPtr.Zero, 0, 0x10000000 /* GENERIC_ALL */, IntPtr.Zero);
      if (desktop == IntPtr.Zero) throw new Win32Exception();
    }
    var si = new STARTUPINFO(); si.cb = Marshal.SizeOf(typeof(STARTUPINFO)); si.lpDesktop = "WinSta0\\VelocityBench";
    var cmd = new StringBuilder("\"" + exe + "\" " + arguments);
    PROCESS_INFORMATION pi;
    if (!CreateProcess(exe, cmd, IntPtr.Zero, IntPtr.Zero, false, 0x00004000 /* BELOW_NORMAL_PRIORITY_CLASS */, IntPtr.Zero, System.IO.Path.GetDirectoryName(exe), ref si, out pi))
      throw new Win32Exception();
    CloseHandle(pi.hThread); CloseHandle(pi.hProcess);
    return pi.pid;
  }
}
'@
}

function Start-OnBenchDesktop([string]$Exe, [string[]]$Arguments) {
    $quoted = @($Arguments | Where-Object { $_ } | ForEach-Object { if ($_ -match '\s') { '"' + $_ + '"' } else { $_ } }) -join ' '
    [System.Diagnostics.Process]::GetProcessById([BenchDesktop]::Start((Resolve-Path $Exe).Path, $quoted))
}
