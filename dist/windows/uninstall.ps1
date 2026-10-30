# Removes ELE Messenger for this user. Messages and the passphrase stay in
# the mesh service's own folder; delete that too to forget this machine.
$dir = Join-Path $env:LOCALAPPDATA 'Programs\ELE Messenger'
$menu = Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs'
Get-CimInstance Win32_Process -Filter "Name = 'powershell.exe'" |
    Where-Object { $_.CommandLine -like '*elemesh.exe*run*' } |
    ForEach-Object { Stop-Process -Id $_.ProcessId -Force }
Get-Process ele, elemesh -ErrorAction SilentlyContinue | Stop-Process -Force
Remove-Item (Join-Path $menu 'ELE Messenger.lnk'), (Join-Path $menu 'Startup\ELE Mesh service.lnk') -ErrorAction SilentlyContinue
Set-Location $env:TEMP
Remove-Item $dir -Recurse -Force -ErrorAction SilentlyContinue
Write-Output 'ELE Messenger removed.'
