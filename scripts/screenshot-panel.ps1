# Dev helper: opens Quota's panel through its Holster route (-open) and screenshots the window.
# Usage: pwsh scripts/screenshot-panel.ps1 -out panel.png -open   (Quota must be running)
param([string]$out, [switch]$open, [int]$wait = 1400)
if ($open) {
  Invoke-RestMethod -Method Post -Uri http://127.0.0.1:47333/holster/open -ContentType application/json -Body '{"edge":"right","anchorY":420}' | Out-Null
}
Start-Sleep -Milliseconds $wait
Add-Type -AssemblyName System.Drawing
Add-Type @"
using System; using System.Runtime.InteropServices;
public class W { [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r); public struct RECT { public int L, T, R, B; } }
"@
$p = Get-Process quota | Select-Object -First 1
$r = New-Object W+RECT; [W]::GetWindowRect($p.MainWindowHandle, [ref]$r) | Out-Null
"rect: $($r.L),$($r.T) $($r.R - $r.L)x$($r.B - $r.T)"
$pad = 14
$w = $r.R - $r.L + 2 * $pad; $h = $r.B - $r.T + 2 * $pad
$bmp = New-Object System.Drawing.Bitmap $w, $h
$g = [System.Drawing.Graphics]::FromImage($bmp); $g.CopyFromScreen($r.L - $pad, $r.T - $pad, 0, 0, $bmp.Size)
$bmp.Save($out); $g.Dispose(); $bmp.Dispose()
