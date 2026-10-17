# tools/screenshot.ps1
# Capture the running asset-library-desktop window to a PNG without depending on
# screen position or z-order (PrintWindow, so nothing can occlude it), and without
# synthesising mouse clicks (those are unreliable under an occluded desktop).
#
#   powershell -ExecutionPolicy Bypass -File tools/screenshot.ps1 -Out grid.png
#   powershell -ExecutionPolicy Bypass -File tools/screenshot.ps1 -View cart    -Out cart.png
#   powershell -ExecutionPolicy Bypass -File tools/screenshot.ps1 -View rig     -Out rig.png
#   powershell -ExecutionPolicy Bypass -File tools/screenshot.ps1 -View blender -Out blender.png
#
# -View uses the app's ALD_DEBUG_VIEW hook, which invokes the matching callback at
# startup, so any surface can be captured deterministically.
param(
    [string]$Exe = "$PSScriptRoot\..\target\debug\asset-library-desktop.exe",
    [string]$Out = "$env:TEMP\ald-shot.png",
    [ValidateSet('grid', 'cart', 'rig', 'model', 'cat', 'blender', 'unity', 'painter')][string]$View = '',
    [int]$Wait = 10000,
    [switch]$Offscreen,
    [switch]$Keep
)

$ErrorActionPreference = 'Stop'
if (-not (Test-Path $Exe)) { throw "exe not found: $Exe (run cargo build first)" }

Add-Type @"
using System;
using System.Runtime.InteropServices;
public struct SHOTRECT { public int L, T, Rt, B; }
public class ShotU {
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out SHOTRECT r);
  [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr a, int x, int y, int w, int ht, uint f);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr hdc, uint flags);
  [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc cb, IntPtr l);
  [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr h);
  delegate bool EnumProc(IntPtr h, IntPtr l);
  // MainWindowHandle can point at a hidden 16x16 placeholder while Slint is still
  // starting up, so pick the largest visible top-level window owned by the process.
  public static IntPtr FindMainWindow(uint pid) {
    IntPtr best = IntPtr.Zero; int bestArea = 0;
    EnumWindows(delegate(IntPtr hw, IntPtr l) {
      uint p; GetWindowThreadProcessId(hw, out p);
      if (p != pid || !IsWindowVisible(hw)) return true;
      SHOTRECT r; GetWindowRect(hw, out r);
      int w = r.Rt - r.L, ht = r.B - r.T;
      if (w > 400 && ht > 300 && w * ht > bestArea) { bestArea = w * ht; best = hw; }
      return true;
    }, IntPtr.Zero);
    return best;
  }
}
"@
Add-Type -AssemblyName System.Drawing

if ($View) { $env:ALD_DEBUG_VIEW = $View } else { Remove-Item Env:\ALD_DEBUG_VIEW -ErrorAction SilentlyContinue }

$proc = Start-Process -FilePath $Exe -PassThru
$h = [IntPtr]::Zero
$deadline = (Get-Date).AddMilliseconds($Wait)
while ((Get-Date) -lt $deadline) {
    Start-Sleep -Milliseconds 250
    $proc.Refresh()
    if ($proc.HasExited) { throw "app exited before showing a window (code $($proc.ExitCode))" }
    $h = [ShotU]::FindMainWindow([uint32]$proc.Id)
    if ($h -ne [IntPtr]::Zero) { break }
}
if ($h -eq [IntPtr]::Zero) { Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue; throw "no window appeared within ${Wait}ms" }
Start-Sleep -Milliseconds 1500

# Foreground + real screen capture: DWM's acrylic/mica backdrop is composited by the
# OS, so only a screen grab can show it (PrintWindow only sees the window's own
# drawing). HWND_TOPMOST (-1) + SWP_NOSIZE (0x1) + SWP_SHOWWINDOW (0x40).
[void][ShotU]::SetWindowPos($h, [IntPtr](-1), 40, 40, 0, 0, 0x0001 -bor 0x0040)
[void][ShotU]::SetForegroundWindow($h)
Start-Sleep -Milliseconds 900

$r = New-Object SHOTRECT
[void][ShotU]::GetWindowRect($h, [ref]$r)
$w = $r.Rt - $r.L
$ht = $r.B - $r.T
$bmp = New-Object System.Drawing.Bitmap $w, $ht
$g = [System.Drawing.Graphics]::FromImage($bmp)
$hdc = $g.GetHdc()
if ($Offscreen) {
  [void][ShotU]::PrintWindow($h, $hdc, 2)
  $g.ReleaseHdc($hdc)
} else {
  $g.ReleaseHdc($hdc)
  $g.CopyFromScreen($r.L, $r.T, 0, 0, (New-Object System.Drawing.Size $w, $ht))
}

$dir = Split-Path -Parent $Out
if ($dir -and -not (Test-Path $dir)) { New-Item -ItemType Directory -Force -Path $dir | Out-Null }
$bmp.Save($Out, [System.Drawing.Imaging.ImageFormat]::Png)
$g.Dispose(); $bmp.Dispose()

if (-not $Keep -and -not $proc.HasExited) {
  # Clear topmost FIRST: if the kill below ever fails, an orphaned always-on-top
  # window would otherwise stay pinned above everything.
  [void][ShotU]::SetWindowPos($h, [IntPtr](-2), 0, 0, 0, 0, 0x0001 -bor 0x0002 -bor 0x0010)
  Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue
  Start-Sleep -Milliseconds 300
  if (Get-Process -Id $proc.Id -ErrorAction SilentlyContinue) { Stop-Process -Id $proc.Id -Force }
}
Write-Output "captured ${w}x${ht} (view '$View') -> $Out"
