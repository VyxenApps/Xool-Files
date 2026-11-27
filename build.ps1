param([switch]$Dev, [switch]$Check, [switch]$Run, [switch]$Test, [switch]$NoCopy)
$ErrorActionPreference = 'Stop'

$local = Join-Path $PSScriptRoot 'build.local.json'
$config = if (Test-Path -LiteralPath $local) { Get-Content -Raw -LiteralPath $local | ConvertFrom-Json } else { $null }

$cargo = $null
if ($config -and $config.cargo -and (Test-Path -LiteralPath $config.cargo)) {
    $cargo = $config.cargo
} else {
    $found = Get-Command cargo -ErrorAction SilentlyContinue
    if ($found) { $cargo = $found.Source }
}
if (-not $cargo) { throw 'Instala Rust desde https://rustup.rs o define la ruta de cargo en build.local.json.' }

$env:PATH = (Split-Path -Parent $cargo) + ';' + $env:PATH
if ($config -and $config.cargo_home) { $env:CARGO_HOME = $config.cargo_home }
$env:CARGO_TARGET_DIR = Join-Path $PSScriptRoot 'target'

$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
$vsRoot = & $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
if (-not $vsRoot) { throw 'Se necesitan las herramientas C++ de Visual Studio.' }
$msvc = Get-ChildItem -LiteralPath (Join-Path $vsRoot 'VC\Tools\MSVC') -Directory | Sort-Object Name -Descending | Select-Object -First 1
$sdkRoot = Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10'
$sdk = Get-ChildItem -LiteralPath (Join-Path $sdkRoot 'Lib') -Directory | Sort-Object Name -Descending | Select-Object -First 1
$sdkBin = Join-Path $sdkRoot ('bin\' + $sdk.Name + '\x64')
$linker = Join-Path $msvc.FullName 'bin\Hostx64\x64\link.exe'

$env:CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER = $linker
$env:PATH = (Split-Path -Parent $linker) + ';' + $sdkBin + ';' + $env:PATH
$env:INCLUDE = (Join-Path $msvc.FullName 'include') + ';' + (Join-Path $sdkRoot ('Include\' + $sdk.Name + '\ucrt')) + ';' + (Join-Path $sdkRoot ('Include\' + $sdk.Name + '\shared')) + ';' + (Join-Path $sdkRoot ('Include\' + $sdk.Name + '\um'))
$env:LIB = (Join-Path $msvc.FullName 'lib\x64') + ';' + (Join-Path $sdk.FullName 'ucrt\x64') + ';' + (Join-Path $sdk.FullName 'um\x64')
$fxc = Join-Path $sdkBin 'fxc.exe'
if (Test-Path -LiteralPath $fxc) { $env:GPUI_FXC_PATH = $fxc }
$env:RUSTFLAGS = '-C target-feature=+crt-static'

Push-Location $PSScriptRoot
try {
    if ($Test) { & $cargo test --release --locked; if ($LASTEXITCODE -ne 0) { throw 'Las pruebas fallaron.' }; return }
    if ($Check) {
        & $cargo check --locked
    } elseif ($Dev) {
        & $cargo build --locked
    } else {
        & $cargo build --release --locked
        if ($LASTEXITCODE -ne 0) { throw 'La compilación falló.' }
        $releaseBinary = Join-Path $PSScriptRoot 'target\release\xool-files.exe'
        if (-not $NoCopy) {
            $releaseCopy = Join-Path $PSScriptRoot 'Xool Files.exe'
            Copy-Item -LiteralPath $releaseBinary -Destination $releaseCopy -Force
            $releaseBinary = $releaseCopy
        }
        Get-Item -LiteralPath $releaseBinary | Select-Object Name, Length, LastWriteTime
    }
    if ($LASTEXITCODE -ne 0) { throw 'La compilación falló.' }
    if ($Run) { $binary = if ($Dev) { 'target\debug\xool-files.exe' } else { 'target\release\xool-files.exe' }; & (Join-Path $PSScriptRoot $binary) }
} finally {
    Pop-Location
}

