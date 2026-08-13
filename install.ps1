# sparkd installer for Windows PowerShell
# Usage: powershell -ExecutionPolicy Bypass -WindowStyle Hidden -Command "Set-Location $env:TEMP; Invoke-WebRequest https://raw.githubusercontent.com/Laynester/spark-dumper/main/install.ps1 -OutFile sparkd-install.ps1; powershell -ExecutionPolicy Bypass -File sparkd-install.ps1"

$repo = "Laynester/spark-dumper"
$version = if ($env:SPARKD_VERSION) { $env:SPARKD_VERSION } else { "latest" }

$base = if ($version -eq "latest") {
    "https://github.com/$repo/releases/latest/download"
} else {
    "https://github.com/$repo/releases/download/$version"
}
$asset = "sparkd-$version-windows-x86_64.zip"
$url = "$base/$asset"

Write-Host "downloading $url"
Invoke-WebRequest -Uri $url -OutFile "$env:TEMP\sparkd.zip"

$dest = "$env:USERPROFILE\.local\bin"
New-Item -ItemType Directory -Force -Path $dest | Out-Null

Expand-Archive -Force -Path "$env:TEMP\sparkd.zip" -DestinationPath "$env:TEMP\sparkd-extract"
Copy-Item -Force "$env:TEMP\sparkd-extract\sparkd\sparkd.exe" "$dest\sparkd.exe"

Write-Host "installed sparkd to $dest"
Write-Host "run 'sparkd --help' to get started (add $dest to your PATH if needed)"