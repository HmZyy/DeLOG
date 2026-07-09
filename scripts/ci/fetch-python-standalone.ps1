param([Parameter(Mandatory)][string]$Dest)
$ErrorActionPreference = "Stop"

$PbsTag = "20240814"
$PyVersion = "3.12.5"
$NumpyVersion = "2.1.1"

$url = "https://github.com/astral-sh/python-build-standalone/releases/download/$PbsTag/cpython-$PyVersion+$PbsTag-x86_64-pc-windows-msvc-install_only.tar.gz"
$tmp = New-Item -ItemType Directory -Path (Join-Path $env:RUNNER_TEMP "pbs") -Force
Invoke-WebRequest -Uri $url -OutFile "$tmp\py.tar.gz"
tar -xzf "$tmp\py.tar.gz" -C "$tmp"          # extracts a `python\` dir
# CI passes <repo>\staging\python; ensure the parent exists before the move.
$parent = Split-Path -Parent $Dest
if ($parent -and -not (Test-Path $parent)) { New-Item -ItemType Directory -Force -Path $parent | Out-Null }
if (Test-Path $Dest) { Remove-Item -Recurse -Force $Dest }
Move-Item "$tmp\python" $Dest

$py = Join-Path $Dest "python.exe"
& $py -m pip install --no-cache-dir "numpy==$NumpyVersion"

# Trim caches, stdlib tests, and pip/ensurepip.
Get-ChildItem -Path $Dest -Recurse -Directory -Filter "__pycache__" | Remove-Item -Recurse -Force
Remove-Item -Recurse -Force "$Dest\Lib\test","$Dest\Lib\tests" -ErrorAction SilentlyContinue
Remove-Item -Recurse -Force "$Dest\Lib\site-packages\pip","$Dest\Lib\ensurepip" -ErrorAction SilentlyContinue

"PYO3_PYTHON=$py" | Out-File -FilePath $env:GITHUB_ENV -Append
