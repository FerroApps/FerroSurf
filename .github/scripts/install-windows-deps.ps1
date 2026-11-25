# Build dependencies of Servo (and therefore FerroSurf) on a GitHub Windows runner.
# SpiderMonkey (mozjs) needs LLVM/clang (bindgen), CMake, Python, GNU make and
# MozillaBuild (the POSIX tools its build system uses), located via MOZTOOLS_PATH.
$ErrorActionPreference = "Stop"
choco install -y llvm make nasm --no-progress
choco install -y cmake --installargs 'ADD_CMAKE_TO_PATH=System' --no-progress

$url = "https://ftp.mozilla.org/pub/mozilla/libraries/win32/MozillaBuildSetup-4.0.exe"
Invoke-WebRequest -Uri $url -OutFile "$env:RUNNER_TEMP\MozillaBuildSetup.exe"
Start-Process -FilePath "$env:RUNNER_TEMP\MozillaBuildSetup.exe" -ArgumentList "/S" -Wait
if (-not (Test-Path "C:\mozilla-build")) { throw "MozillaBuild install failed" }

"MOZILLABUILD=C:\mozilla-build" | Out-File -FilePath $env:GITHUB_ENV -Append -Encoding utf8
"MOZTOOLS_PATH=C:\mozilla-build\msys2\usr\bin" | Out-File -FilePath $env:GITHUB_ENV -Append -Encoding utf8
"LIBCLANG_PATH=C:\Program Files\LLVM\bin" | Out-File -FilePath $env:GITHUB_ENV -Append -Encoding utf8
"C:\Program Files\LLVM\bin" | Out-File -FilePath $env:GITHUB_PATH -Append -Encoding utf8

# The runner's MSVC STL may be newer than the LLVM from Chocolatey and refuses older clang
# ("STL1000: Unexpected compiler version, expected Clang NN or newer"). Allow the mismatch for
# bindgen (libclang) and for clang-cl / cl (both read `_CL_`).
"BINDGEN_EXTRA_CLANG_ARGS=-D_ALLOW_COMPILER_AND_STL_VERSION_MISMATCH" | Out-File -FilePath $env:GITHUB_ENV -Append -Encoding utf8
"_CL_=/D_ALLOW_COMPILER_AND_STL_VERSION_MISMATCH" | Out-File -FilePath $env:GITHUB_ENV -Append -Encoding utf8
