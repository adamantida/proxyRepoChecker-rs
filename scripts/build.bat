@echo off
setlocal EnableExtensions DisableDelayedExpansion
set "PLATFORM="
set "PLATFORM_EXPLICIT=0"
set "ARCH="
set "INSTALL_TARGET=0"
set "FAILURES=0"

:parse
if "%~1"=="" goto parsed
if /i "%~1"=="--help" goto help
if /i "%~1"=="-h" goto help
if /i "%~1"=="--platform" goto get_platform
if /i "%~1"=="-p" goto get_platform
if /i "%~1"=="--arch" goto get_arch
if /i "%~1"=="-a" goto get_arch
if /i "%~1"=="--install-target" goto install_flag
if /i "%~1"=="linux" goto positional_platform
if /i "%~1"=="macos" goto positional_platform
if /i "%~1"=="windows" goto positional_platform
if /i "%~1"=="all" goto positional_platform
echo Error: unknown argument. Use --help.
exit /b 2

:get_platform
if "%~2"=="" (echo Error: missing platform value.& exit /b 2)
if not "%PLATFORM%"=="" (echo Error: platform specified more than once.& exit /b 2)
set "PLATFORM=%~2"
set "PLATFORM_EXPLICIT=1"
shift
shift
goto parse

:get_arch
if "%~2"=="" (echo Error: missing architecture value.& exit /b 2)
if not "%ARCH%"=="" (echo Error: architecture specified more than once.& exit /b 2)
set "ARCH=%~2"
shift
shift
goto parse

:install_flag
set "INSTALL_TARGET=1"
shift
goto parse

:positional_platform
if not "%PLATFORM%"=="" (echo Error: platform specified more than once.& exit /b 2)
set "PLATFORM=%~1"
set "PLATFORM_EXPLICIT=1"
shift
goto parse

:help
echo Usage: scripts\build.bat [linux^|macos^|windows^|all] [options]
echo.
echo Build proxy-rs in release mode. The default platform is the current host.
echo Options:
echo   -p, --platform PLATFORM  linux, macos, windows, or all
echo   -a, --arch ARCH          x86_64 or aarch64 (default: host architecture)
echo       --install-target     Run rustup target add for non-native targets
echo   -h, --help               Show this help
echo.
echo Examples:
echo   scripts\build.bat
echo   scripts\build.bat windows --arch x86_64
echo   scripts\build.bat all --arch aarch64 --install-target
echo.
echo Native builds use target\release\. Cross builds use target\^<rust-target^>\release\.
echo rustup installs only Rust standard libraries, not linkers or SDKs.
echo Windows cross builds require a compatible linker; macOS builds require Apple tooling.
echo Builds continue after failures and return non-zero if any requested build fails.
exit /b 0

:parsed
for /f "tokens=2 delims=: " %%H in ('rustc -vV 2^>nul ^| findstr /b "host:"') do set "HOST=%%H"
if not defined HOST (echo Error: rustc was not found or its host target could not be determined.& exit /b 2)
for /f "tokens=1 delims=-" %%A in ("%HOST%") do set "HOST_ARCH=%%A"
if not defined PLATFORM goto detect_default_platform
goto platform_ready

:platform_ready
if "%ARCH%"=="" set "ARCH=%HOST_ARCH%"
if /i not "%ARCH%"=="x86_64" if /i not "%ARCH%"=="aarch64" (echo Error: unsupported architecture. Choose x86_64 or aarch64.& exit /b 2)
if /i not "%PLATFORM%"=="linux" if /i not "%PLATFORM%"=="macos" if /i not "%PLATFORM%"=="windows" if /i not "%PLATFORM%"=="all" (echo Error: unsupported platform.& exit /b 2)

for %%I in ("%~dp0..") do set "ROOT=%%~fI"
pushd "%ROOT%" >nul || (echo Error: cannot enter repository root.& exit /b 2)
set "TARGET_ROOT=%CARGO_TARGET_DIR%"
if not defined TARGET_ROOT set "TARGET_ROOT=target"
for %%I in ("%TARGET_ROOT%") do set "TARGET_ROOT=%%~fI"
where cargo >nul 2>nul || (echo Error: cargo was not found in PATH.& popd & exit /b 2)
if /i "%PLATFORM%"=="all" (
  for %%P in (linux macos windows) do call :build_platform %%P
) else (
  call :build_platform %PLATFORM%
)
goto finished

:detect_default_platform
echo(%HOST%| findstr /c:"-apple-darwin" >nul && set "PLATFORM=macos"
echo(%HOST%| findstr /c:"-windows-" >nul && set "PLATFORM=windows"
echo(%HOST%| findstr /c:"-linux-" >nul && set "PLATFORM=linux"
if not defined PLATFORM (echo Error: unsupported host target "%HOST%". Select --platform explicitly.& exit /b 2)
if "%ARCH%"=="" set "ARCH=%HOST_ARCH%"
if /i not "%ARCH%"=="x86_64" if /i not "%ARCH%"=="aarch64" (echo Error: unsupported architecture "%ARCH%". Choose x86_64 or aarch64.& exit /b 2)
goto platform_ready

:build_platform
set "P=%~1"
set "T="
if /i "%P%"=="linux" if /i "%ARCH%"=="x86_64" set "T=x86_64-unknown-linux-gnu"
if /i "%P%"=="linux" if /i "%ARCH%"=="aarch64" set "T=aarch64-unknown-linux-gnu"
if /i "%P%"=="macos" if /i "%ARCH%"=="x86_64" set "T=x86_64-apple-darwin"
if /i "%P%"=="macos" if /i "%ARCH%"=="aarch64" set "T=aarch64-apple-darwin"
if /i "%P%"=="windows" if /i "%ARCH%"=="x86_64" set "T=x86_64-pc-windows-gnu"
if /i "%P%"=="windows" if /i "%ARCH%"=="aarch64" set "T=aarch64-pc-windows-msvc"
if /i "%HOST_ARCH%"=="%ARCH%" if /i "%P%"=="linux" echo(%HOST%| findstr /c:"-linux-" >nul && set "T=%HOST%"
if /i "%HOST_ARCH%"=="%ARCH%" if /i "%P%"=="macos" echo(%HOST%| findstr /c:"-apple-darwin" >nul && set "T=%HOST%"
if /i "%HOST_ARCH%"=="%ARCH%" if /i "%P%"=="windows" echo(%HOST%| findstr /c:"-windows-" >nul && set "T=%HOST%"
call :build "%P%" "%T%"
exit /b 0

:build
set "P=%~1"
set "T=%~2"
set "TARGET_PLATFORM="
set "NATIVE=0"
if /i "%T%"=="%HOST%" set "NATIVE=1"
if "%NATIVE%"=="1" (echo.^> Building %P% (%T%) [native]) else echo.^> Building %P% (%T%) [cross]
if not "%NATIVE%"=="1" if "%INSTALL_TARGET%"=="1" (
  where rustup >nul 2>nul || (echo Error: --install-target requested but rustup was not found.& set /a FAILURES+=1 & exit /b 0)
  rustup target add "%T%"
  if errorlevel 1 (echo Error: rustup could not install target %T%.& set /a FAILURES+=1 & exit /b 0)
)
if "%NATIVE%"=="1" (
  cargo build --workspace --release
) else (
  cargo build --workspace --release --target "%T%"
)
if errorlevel 1 (echo Error: build failed for %P% (%T%).& set /a FAILURES+=1 & exit /b 0)
set "OUT=%TARGET_ROOT%\%T%\release"
if "%NATIVE%"=="1" set "OUT=%TARGET_ROOT%\release"
set "BIN=proxy-rs"
if /i "%P%"=="windows" set "BIN=proxy-rs.exe"
if not exist "%OUT%\%BIN%" (echo Error: build succeeded but binary is missing: %OUT%\%BIN%.& set /a FAILURES+=1 & exit /b 0)
echo Success: "%OUT%\%BIN%"
exit /b 0

:finished
popd
if not "%FAILURES%"=="0" (echo Build finished with %FAILURES% failure(s).& exit /b 1)
echo All requested builds succeeded.
exit /b 0
