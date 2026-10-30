# Installs ELE Messenger for this user, no administrator needed:
# - the app and the mesh service in %LOCALAPPDATA%\Programs\ELE Messenger,
# - "ELE Messenger" in the Start menu,
# - the mesh service started at sign-in, with no window, and started now.
# Run from the unzipped folder:
#   powershell -ExecutionPolicy Bypass -File install.ps1
$ErrorActionPreference = 'Stop'
$from = $PSScriptRoot
$dir = Join-Path $env:LOCALAPPDATA 'Programs\ELE Messenger'
$menu = Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs'
$startup = Join-Path $menu 'Startup'

Get-Process ele, elemesh -ErrorAction SilentlyContinue | Stop-Process -Force
Get-CimInstance Win32_Process -Filter "Name = 'powershell.exe'" |
    Where-Object { $_.CommandLine -like '*elemesh.exe*run*' } |
    ForEach-Object { Stop-Process -Id $_.ProcessId -Force }
New-Item -ItemType Directory -Force $dir | Out-Null
Copy-Item (Join-Path $from 'ele.exe'), (Join-Path $from 'elemesh.exe'), (Join-Path $from 'ele.ico'), (Join-Path $from 'uninstall.ps1') $dir -Force

# The service exits to apply new settings (a new passphrase); this loop
# brings it back, as systemd and launchd do elsewhere. conhost --headless
# keeps its console hidden.
$mesh = Join-Path $dir 'elemesh.exe'
$loop = "while (`$true) { & '$mesh' run; Start-Sleep 5 }"
$shell = New-Object -ComObject WScript.Shell
$s = $shell.CreateShortcut((Join-Path $menu 'ELE Messenger.lnk'))
$s.TargetPath = Join-Path $dir 'ele.exe'; $s.WorkingDirectory = $dir
$s.IconLocation = Join-Path $dir 'ele.ico'; $s.Description = 'Chat with the ELE machines around you'
$s.Save()
$s = $shell.CreateShortcut((Join-Path $startup 'ELE Mesh service.lnk'))
$s.TargetPath = Join-Path $env:WINDIR 'System32\conhost.exe'
$s.Arguments = "--headless powershell.exe -NoProfile -WindowStyle Hidden -Command `"$loop`""
$s.WorkingDirectory = $dir; $s.IconLocation = Join-Path $dir 'ele.ico'; $s.Description = 'ELE Mesh service'
$s.Save()
Start-Process -FilePath $s.TargetPath -ArgumentList $s.Arguments -WorkingDirectory $dir -WindowStyle Hidden
Write-Output "Installed ELE Messenger in $dir. Open it from the Start menu."
Write-Output "Allow ELE through the firewall if Windows asks."
