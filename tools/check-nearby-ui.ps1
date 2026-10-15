param([Parameter(Mandatory=$true)][string]$Exe)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes, System.Windows.Forms
Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class NearbyNative {
 [DllImport("kernel32.dll")] public static extern uint SetThreadExecutionState(uint flags);
 [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int w, int height, uint flags);
 [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int command);
 [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
 [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
 [DllImport("user32.dll")] public static extern void mouse_event(uint flags, uint x, uint y, uint data, UIntPtr extra);
 [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint msg, IntPtr w, IntPtr l);
 [DllImport("user32.dll")] public static extern IntPtr SetThreadDpiAwarenessContext(IntPtr value);
}
'@
[void][NearbyNative]::SetThreadDpiAwarenessContext([IntPtr](-4))
[void][NearbyNative]::SetThreadExecutionState([uint32]2147483651)
$root = Split-Path $PSScriptRoot -Parent
$out = Join-Path $root '.work/oct11-sharing/native-ui'
[IO.Directory]::CreateDirectory($out) | Out-Null
# Isolated fixture: no access to the installed user's library or music folder.
@'
from pathlib import Path
import sqlite3, wave
p=Path('.work/oct11-sharing/native-ui'); music=p/'music'; music.mkdir(parents=True,exist_ok=True)
for suffix in ['', '-wal', '-shm']:
 (p/f'library.sqlite3{suffix}').unlink(missing_ok=True)
for name in ['Example.wav','Example 2.wav','Example 3.wav']:
 with wave.open(str(music/name),'wb') as f:
  f.setparams((1,2,8000,0,'NONE','not compressed')); f.writeframes(bytes(60*8000*2))
db=sqlite3.connect(p/'library.sqlite3'); db.execute('CREATE TABLE IF NOT EXISTS settings(key TEXT PRIMARY KEY,value TEXT NOT NULL)')
for k,v in {'guide_seen':'1','theme_hint_seen':'1','glass':'0','rail_collapsed':'0','low_power':'1','music_folder':str(music.resolve()),'download_folder':str(music.resolve())}.items():
 db.execute('INSERT OR REPLACE INTO settings VALUES(?,?)',(k,v))
db.commit(); db.close()
'@ | python -
if ($LASTEXITCODE -ne 0) { throw 'Fixture failed.' }
$env:MONOLITH_DATA_DIR = $out
$env:SLINT_SCALE_FACTOR = '1'
$app = Start-Process -FilePath ([IO.Path]::GetFullPath($Exe)) -WindowStyle Hidden -PassThru
$results = @()
function Find-Window([string]$title) {
 $rootElement = [System.Windows.Automation.AutomationElement]::RootElement
 $condition = [System.Windows.Automation.AndCondition]::new(
  [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::ProcessIdProperty,$app.Id),
  [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::NameProperty,$title))
 $until = [DateTime]::UtcNow.AddSeconds(20)
 do {
  $found = $rootElement.FindFirst([System.Windows.Automation.TreeScope]::Children,$condition)
  if ($found) { return $found }
  Start-Sleep -Milliseconds 150
 } while ([DateTime]::UtcNow -lt $until)
 throw "Window not found: $title"
}
function Control($window, [string]$name) {
 $condition = [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::NameProperty,$name)
 $until = [DateTime]::UtcNow.AddSeconds(4)
 do {
  $elements = $window.FindAll([System.Windows.Automation.TreeScope]::Descendants,$condition)
  if ($elements.Count -gt 0) { break }
  Start-Sleep -Milliseconds 100
 } while ([DateTime]::UtcNow -lt $until)
 if ($elements.Count -eq 0) {
  $window.FindAll([System.Windows.Automation.TreeScope]::Descendants,[System.Windows.Automation.Condition]::TrueCondition) | ForEach-Object { $_.Current.Name } | Set-Content -LiteralPath (Join-Path $out 'failed-controls.txt')
  try { Capture $window 'failed-control' } catch { Write-Warning "Diagnostic capture unavailable: $_" }
  throw "Accessible control missing: $name"
 }
 foreach($element in $elements) {
  if($element.Current.ControlType -eq [System.Windows.Automation.ControlType]::Button) { return $element }
 }
 return $elements[0]
}
function Invoke-Control($window, [string]$name) {
 $element = Control $window $name
 if(-not $element.Current.IsEnabled) { throw "Disabled control: $name" }
 $pattern=$null
 if($element.TryGetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern,[ref]$pattern)) {
  try { $pattern.Invoke() } catch {
   # Slint can replace an accessible node during a successful action. Never
   # retry it (that could toggle twice); following control/DB assertions must
   # prove its postcondition. All other provider failures remain failures.
   if($name -notin @('Create','Close guide','Cancel','Remove','Create playlist from selected songs')) { throw "UIA action failed ($name): $_" }
   Write-Warning "UIA replaced control during $name; checking its postcondition."
  }
 } elseif($element.TryGetCurrentPattern([System.Windows.Automation.TogglePattern]::Pattern,[ref]$pattern)) { $pattern.Toggle() }
 else {
  $r=$element.Current.BoundingRectangle
  $bounds=$window.Current.BoundingRectangle
  $x=[int]($r.Left+$r.Width/2); $y=[int]($r.Top+$r.Height/2)
  if($r.Width -lt 4 -or $r.Height -lt 4 -or $x -lt $bounds.Left -or $x -gt $bounds.Right -or $y -lt $bounds.Top -or $y -gt $bounds.Bottom) { throw "Unclickable fixture control bounds: $name" }
  [void][NearbyNative]::SetForegroundWindow([IntPtr]$window.Current.NativeWindowHandle)
  [void][NearbyNative]::SetCursorPos($x,$y)
  [NearbyNative]::mouse_event(2,0,0,0,[UIntPtr]::Zero)
  [NearbyNative]::mouse_event(4,0,0,0,[UIntPtr]::Zero)
 }
 Start-Sleep -Milliseconds 300
}
function Capture($window,[string]$name) {
 # Let window/page/control transitions settle before inspecting pixels.
 Start-Sleep -Milliseconds 700
 $r=$window.Current.BoundingRectangle
 $env:MONOLITH_TEST_SHOT = Join-Path $out "$name.png"
 $env:MONOLITH_TEST_RECT = @([int]$r.Left,[int]$r.Top,[int]$r.Right,[int]$r.Bottom) -join ','
 @'
import os
from PIL import ImageGrab
box=tuple(map(int,os.environ['MONOLITH_TEST_RECT'].split(',')))
im=ImageGrab.grab(bbox=box)
if not any(max(band) for band in im.getextrema()):
 from pathlib import Path
 Path(os.environ['MONOLITH_TEST_SHOT']).unlink(missing_ok=True)
 print('WARNING: desktop capture unavailable (black frame); native actions and rendered guide checks continue.')
else:
 im.save(os.environ['MONOLITH_TEST_SHOT'])
assert im.width>=400 and im.height>=350
'@ | python -
 if ($LASTEXITCODE -ne 0) { throw 'Capture failed.' }
}
function Assert-NoSharingSockets {
 $until=[DateTime]::UtcNow.AddSeconds(6)
 do {
  $tcp=@(Get-NetTCPConnection -OwningProcess $app.Id -State Listen -ErrorAction SilentlyContinue)
  $udp=@(Get-NetUDPEndpoint -OwningProcess $app.Id -ErrorAction SilentlyContinue)
  if($tcp.Count -eq 0 -and $udp.Count -eq 0) { return }
  Start-Sleep -Milliseconds 200
 } while([DateTime]::UtcNow -lt $until)
 throw 'Sharing sockets must close when sharing is inactive.'
}
try {
 $main=Find-Window 'Monolith'
 [void][NearbyNative]::ShowWindow([IntPtr]$main.Current.NativeWindowHandle,9)
 [void][NearbyNative]::SetWindowPos([IntPtr]$main.Current.NativeWindowHandle,[IntPtr](-1),20,20,1296,859,0x0040)
 [void][NearbyNative]::SetForegroundWindow([IntPtr]$main.Current.NativeWindowHandle)
 Start-Sleep -Seconds 3
 Assert-NoSharingSockets
 # Regression: the real library title/row still starts the generated WAV.
 Invoke-Control $main 'Example'
 Start-Sleep -Seconds 1
 [void](Control $main 'Pause')
 # Invoke the actual Library entry point, not the sharing-window constructor.
 Invoke-Control $main 'Share music'
 $sharing=Find-Window 'Share music · Monolith'
 [void][NearbyNative]::SetWindowPos([IntPtr]$sharing.Current.NativeWindowHandle,[IntPtr](-1),40,40,636,759,0x0040)
 [void][NearbyNative]::SetForegroundWindow([IntPtr]$sharing.Current.NativeWindowHandle)
 Start-Sleep -Seconds 2
 Capture $sharing 'sharing-chooser'
 Invoke-Control $sharing 'Share via Bluetooth'
 Start-Sleep -Seconds 1
 Invoke-Control $sharing 'Example · Unknown artist'
 $status='No Bluetooth radio available. Enable Bluetooth or connect an adapter, then refresh.'
 [void](Control $sharing $status)
 foreach($name in @('Send song','Receive')) { if((Control $sharing $name).Current.IsEnabled) { throw "No-radio $name must be disabled." } }
 Capture $sharing 'sharing-wide'
 Invoke-Control $sharing 'Refresh nearby devices and songs'
 Start-Sleep -Seconds 1
 [void](Control $sharing $status)
 Capture $main 'library-share-entry'
 [void][NearbyNative]::SetWindowPos([IntPtr]$sharing.Current.NativeWindowHandle,[IntPtr](-1),40,40,436,419,0x0040)
 Start-Sleep -Milliseconds 700
 Capture $sharing 'sharing-minimum'
 # Wi-Fi is independent of Bluetooth hardware and starts only on explicit Receive.
 Invoke-Control $sharing 'Change sharing method'
 Invoke-Control $sharing 'Share via Monolith app'
 Start-Sleep -Seconds 3
 if(-not (Control $sharing 'Receive').Current.IsEnabled) { throw 'Wi-Fi Receive must be available without Bluetooth hardware.' }
 Invoke-Control $sharing 'Receive'
 Start-Sleep -Seconds 1
 [void](Control $sharing 'READY TO RECEIVE')
 if(@(Get-NetUDPEndpoint -OwningProcess $app.Id -LocalPort 45881 -ErrorAction SilentlyContinue).Count -ne 1) { throw 'Explicit Wi-Fi Receive must own its scoped discovery listener.' }
 $names=$sharing.FindAll([System.Windows.Automation.TreeScope]::Descendants,[System.Windows.Automation.Condition]::TrueCondition) | ForEach-Object { $_.Current.Name }
 if($names -contains 'Copy key' -or $names -contains 'Recipient verification key') { throw 'Sharing must not require copying verification codes.' }
 Capture $sharing 'wifi-receiving-minimum'
 Invoke-Control $sharing 'Cancel'
 Start-Sleep -Seconds 1
 Assert-NoSharingSockets
 [void][NearbyNative]::PostMessage([IntPtr]$sharing.Current.NativeWindowHandle,0x10,[IntPtr]::Zero,[IntPtr]::Zero)
 Start-Sleep -Milliseconds 500
 Assert-NoSharingSockets
 [void][NearbyNative]::ShowWindow([IntPtr]$main.Current.NativeWindowHandle,9)
 [void][NearbyNative]::SetWindowPos([IntPtr]$main.Current.NativeWindowHandle,[IntPtr](-1),20,20,436,419,0x0040)
 Start-Sleep -Milliseconds 600
 $entry=Control $main 'Share music'
 if($entry.Current.BoundingRectangle.Right -gt $main.Current.BoundingRectangle.Right) { throw 'Compact sharing entry overflows the main window.' }
 Capture $main 'library-minimum'
 Invoke-Control $main 'Share music'
 $sharing=Find-Window 'Share music · Monolith'
 Start-Sleep -Seconds 1
 Invoke-Control $sharing 'Share via Bluetooth'
 Start-Sleep -Seconds 1
 [void](Control $sharing $status)
 [void][NearbyNative]::PostMessage([IntPtr]$sharing.Current.NativeWindowHandle,0x10,[IntPtr]::Zero,[IntPtr]::Zero)
 Start-Sleep -Milliseconds 500
 Invoke-Control $main 'Select songs'
 Invoke-Control $main 'Select Example'
 [void](Control $main '1 selected')
 Invoke-Control $main 'Show app guide'
 for($step=0;$step -lt 5;$step++) { Invoke-Control $main 'Next' }
 [void](Control $main 'Share your music')
 Capture $main 'sharing-guide-minimum'
 Invoke-Control $main 'Close guide'
 Invoke-Control $main 'Select songs'
 Invoke-Control $main 'Select all visible songs'
 [void](Control $main '3 selected')
 Capture $main 'selection-minimum'
 Invoke-Control $main 'Share selected songs'
 $sharing=Find-Window 'Share music · Monolith'
 Start-Sleep -Milliseconds 600
 Invoke-Control $sharing 'Share via Bluetooth'
 Start-Sleep -Seconds 1
 [void](Control $sharing '3 selected songs. Change this selection in Library or Songs.')
 if((Control $sharing 'Example · Unknown artist').Current.IsEnabled) { throw 'Batch sharing must keep the library selection locked.' }
 [void][NearbyNative]::PostMessage([IntPtr]$sharing.Current.NativeWindowHandle,0x10,[IntPtr]::Zero,[IntPtr]::Zero)
 Start-Sleep -Milliseconds 500
 Invoke-Control $main 'Create playlist from selected songs'
 $field=Control $main 'New playlist name'
 $field.SetFocus()
 [System.Windows.Forms.SendKeys]::SendWait('Batch test')
 Start-Sleep -Milliseconds 300
 Invoke-Control $main 'Create'
 Start-Sleep -Seconds 1
 Invoke-Control $main 'Select all visible songs'
 Invoke-Control $main 'Remove selected songs from library'
 Invoke-Control $main 'Cancel'
 [void](Control $main '3 selected')
 @'
import sqlite3
from pathlib import Path
p=Path('.work/oct11-sharing/native-ui')
db=sqlite3.connect(p/'library.sqlite3')
assert db.execute('SELECT COUNT(*) FROM tracks').fetchone()[0]==3
assert db.execute('SELECT COUNT(*) FROM playlist_tracks').fetchone()[0]==3
'@ | python -
 if($LASTEXITCODE -ne 0) {throw 'Bulk playlist creation or removal Cancel changed the wrong data.'}
 Invoke-Control $main 'Remove selected songs from library'
 Invoke-Control $main 'Remove'
 Start-Sleep -Seconds 1
 @'
import sqlite3
from pathlib import Path
p=Path('.work/oct11-sharing/native-ui')
db=sqlite3.connect(p/'library.sqlite3')
assert db.execute('SELECT COUNT(*) FROM tracks').fetchone()[0]==0
assert db.execute('SELECT COUNT(*) FROM playlist_tracks').fetchone()[0]==0
assert len(list((p/'music').glob('*.wav')))==3
'@ | python -
 if($LASTEXITCODE -ne 0) {throw 'Confirmed removal failed or original songs were deleted.'}
 Invoke-Control $main 'Finish selecting songs'
 if((Control $main 'Select songs').Current.IsEnabled) { throw 'Empty library Select must be disabled after leaving selection.' }
 Capture $main 'empty-after-removal'
 $results += 'Title playback, sharing chooser, Bluetooth no-radio state, Wi-Fi Receive/Cancel, no sharing sockets before Share/after Cancel or Close, compact layouts, sharing guide, selection boxes/select-all, locked three-song sharing selection, bulk playlist creation, removal Cancel/Confirm and preservation of originals passed.'
 $app.Refresh()
 $results += "Sharing process working set: $([math]::Round($app.WorkingSet64/1MB,1)) MiB; private: $([math]::Round($app.PrivateMemorySize64/1MB,1)) MiB."
 $results | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $out 'result.json')
 Write-Output $results
} finally {
 if (-not $app.HasExited) {
  $main=Find-Window 'Monolith'
  [void][NearbyNative]::PostMessage([IntPtr]$main.Current.NativeWindowHandle,0x10,[IntPtr]::Zero,[IntPtr]::Zero)
  if(-not $app.WaitForExit(15000)) { Stop-Process -Id $app.Id; throw 'App did not exit after Close.' }
 }
 Remove-Item Env:MONOLITH_DATA_DIR,Env:MONOLITH_TEST_SHOT,Env:MONOLITH_TEST_RECT -ErrorAction SilentlyContinue
 [void][NearbyNative]::SetThreadExecutionState([uint32]2147483648)
}
