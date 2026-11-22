param(
  [string]$Exe = "G:\Temp\rust\target\debug\orion-ui.exe",
  [string]$RunRoot = "G:\Temp\tmp\ulw-5d64e22ec37b4b3294613688aae4a45c\capture-runs",
  [string]$Theme = "dark",
  [string]$Density = "comfortable",
  [string]$OutDir = "docs\visual\screenshots"
)

$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.Drawing
Add-Type @"
using System; using System.Text; using System.Runtime.InteropServices;
public static class F {
  public delegate bool E(IntPtr h, IntPtr p);
  [DllImport("user32.dll")] public static extern bool EnumWindows(E cb, IntPtr p);
  [DllImport("user32.dll")] public static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out R r);
  [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr hdc, uint flags);
  [DllImport("user32.dll")] public static extern bool MoveWindow(IntPtr hwnd, int x, int y, int w, int hh, bool repaint);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int cmd);
  [DllImport("user32.dll")] public static extern bool BringWindowToTop(IntPtr h);
  [StructLayout(LayoutKind.Sequential)] public struct R { public int L,T,Rr,B; }
}
"@
function Find-Win {
  $list=[Collections.Generic.List[object]]::new()
  $cb=[F+E]{param($h,$p); if([F]::IsWindowVisible($h)){$sb=[Text.StringBuilder]::new(400);[void][F]::GetWindowText($h,$sb,400);$t=$sb.ToString();if($t -match 'Orion'){$r=New-Object F+R;[void][F]::GetWindowRect($h,[ref]$r);$list.Add([pscustomobject]@{H=$h;T=$t;W=($r.Rr-$r.L);HH=($r.B-$r.T)})}};return $true}
  [void][F]::EnumWindows($cb,[IntPtr]::Zero)
  return ($list | Where-Object {$_.W -ge 500 -and $_.HH -ge 400} | Select-Object -First 1)
}
function Get-Stats($b){$c=@{};$s=0.0;$n=0;for($y=0;$y -lt $b.Height;$y+=6){for($x=0;$x -lt $b.Width;$x+=6){$p=$b.GetPixel($x,$y);$c["$($p.R),$($p.G),$($p.B)"]=1;$s+=($p.R+$p.G+$p.B)/3.0;$n++}};return [pscustomobject]@{Mean=[math]::Round($s/$n,1);Unique=$c.Count}}

New-Item -ItemType Directory -Path $OutDir -Force | Out-Null
$dataDir=Join-Path $RunRoot ('Orion-' + $Theme + '-' + $Density)
New-Item -ItemType Directory -Path $dataDir -Force | Out-Null
Copy-Item -LiteralPath 'C:\Users\Asep Saepuloh\AppData\Roaming\Orion\workspaces.json' -Destination $dataDir -Force -ErrorAction SilentlyContinue
Copy-Item -LiteralPath 'C:\Users\Asep Saepuloh\AppData\Roaming\Orion\push.json' -Destination $dataDir -Force -ErrorAction SilentlyContinue
$prefs=Get-Content -LiteralPath 'C:\Users\Asep Saepuloh\AppData\Roaming\Orion\preferences.json' -Raw | ConvertFrom-Json
$prefs.settings.theme=$Theme
$prefs.settings.density=$Density
$prefs | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath (Join-Path $dataDir 'preferences.json')

$before=@(Get-Process -ErrorAction SilentlyContinue|Where-Object{$_.ProcessName -like 'orion*'}|ForEach-Object{$_.Id})
$oldData=$env:ORION_DATA_DIR; $oldComp=$env:GPUI_DISABLE_DIRECT_COMPOSITION
$env:ORION_DATA_DIR=$dataDir; $env:GPUI_DISABLE_DIRECT_COMPOSITION='1'; $env:RUST_LOG='warn'
$proc=$null
try {
  $proc=Start-Process -FilePath $Exe -PassThru -RedirectStandardError (Join-Path $RunRoot 'stderr.log') -RedirectStandardOutput (Join-Path $RunRoot 'stdout.log')
  Write-Output "PID=$($proc.Id) THEME=$Theme DENSITY=$Density"
  $w=$null;$deadline=(Get-Date).AddSeconds(60)
  while((Get-Date) -lt $deadline){$w=Find-Win;if($w){break};Start-Sleep -Milliseconds 500}
  if(-not $w){throw 'no Orion window'}
  Write-Output "WINDOW=$($w.T) initial $($w.W)x$($w.HH)"
  [void][F]::MoveWindow($w.H,0,0,1280,800,$true)
  [void][F]::ShowWindow($w.H,9);[void][F]::BringWindowToTop($w.H);[void][F]::SetForegroundWindow($w.H)
  Start-Sleep -Milliseconds 700
  $deadline=(Get-Date).AddSeconds(25);$i=0;$best=0
  while((Get-Date) -lt $deadline){
    $i++
    $r=New-Object F+R;[void][F]::GetWindowRect($w.H,[ref]$r)
    $ww=$r.Rr-$r.L;$hh=$r.B-$r.T
    if($ww -lt 100 -or $hh -lt 100){Write-Output "frame ${i}: window gone";break}
    $b=[Drawing.Bitmap]::new($ww,$hh);$g=[Drawing.Graphics]::FromImage($b);$hdc=$g.GetHdc();$ok=[F]::PrintWindow($w.H,$hdc,2);$g.ReleaseHdc($hdc);$g.Dispose()
    $s=Get-Stats $b
    $b2=[Drawing.Bitmap]::new($ww,$hh);$g2=[Drawing.Graphics]::FromImage($b2);$g2.CopyFromScreen($r.L,$r.T,0,0,[Drawing.Size]::new($ww,$hh));$g2.Dispose()
    $s2=Get-Stats $b2
    Write-Output "frame ${i}: PW unique=$($s.Unique) mean=$($s.Mean) | SCREEN unique=$($s2.Unique) mean=$($s2.Mean)"
    if($s2.Unique -gt $best){$best=$s2.Unique;$b2.Save((Join-Path $OutDir ("orion-1280x800-" + $Theme + "-" + $Density + ".png")),[Drawing.Imaging.ImageFormat]::Png)}
    $b.Dispose();$b2.Dispose()
    if($s2.Unique -ge 60){break}
    Start-Sleep -Milliseconds 900
  }
  Write-Output "BEST_SCREEN_UNIQUE=$best"
} finally {
  $after=@(Get-Process -ErrorAction SilentlyContinue|Where-Object{$_.ProcessName -like 'orion*'}|ForEach-Object{$_.Id})
  $new=$after|Where-Object{$_ -notin $before}
  foreach($id in @($proc.Id)+@($new)){Stop-Process -Id $id -Force -ErrorAction SilentlyContinue}
  Write-Output "KILLED=$((@($proc.Id)+@($new)) -join ',')"
  if($null -eq $oldData){Remove-Item Env:ORION_DATA_DIR -ErrorAction SilentlyContinue}else{$env:ORION_DATA_DIR=$oldData}
  if($null -eq $oldComp){Remove-Item Env:GPUI_DISABLE_DIRECT_COMPOSITION -ErrorAction SilentlyContinue}else{$env:GPUI_DISABLE_DIRECT_COMPOSITION=$oldComp}
}

