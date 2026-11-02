@echo off
setlocal

REM ============================================================================
REM  ninerustrouter production build
REM
REM  Order matters: crates/router-server embeds web/dist through rust-embed
REM  (`#[folder = "../../web/dist"]`), which compiles each asset in with
REM  include_bytes!. The backend must therefore be built AFTER the frontend, or
REM  the binary carries a stale (or missing) dashboard. Cargo enforces this too:
REM  the dist files are in router-server's dep-info, so a changed asset forces a
REM  rebuild of that crate.
REM
REM  Output: ninerustrouter-binary\ninerustrouter.exe
REM ============================================================================

cd /d "%~dp0"
set "ROOT=%CD%"

REM Map the Windows architecture onto the CI asset slug (x64/arm64).
set "ARCH=x64"
if /i "%PROCESSOR_ARCHITECTURE%"=="ARM64" set "ARCH=arm64"
set "WEB=%ROOT%\web"
set "OUT=%ROOT%\ninerustrouter-binary"

echo ============================================================
echo  ninerustrouter production build
echo ============================================================

where node >nul 2>nul
if errorlevel 1 (
    echo [ERROR] node not found on PATH.
    exit /b 1
)
where npm >nul 2>nul
if errorlevel 1 (
    echo [ERROR] npm not found on PATH.
    exit /b 1
)
where cargo >nul 2>nul
if errorlevel 1 (
    echo [ERROR] cargo not found on PATH.
    exit /b 1
)

if not exist "%WEB%\package.json" (
    echo [ERROR] web\package.json not found - run this script from the repo root.
    exit /b 1
)

REM ---------------------------------------------------------------- frontend --
REM  `npm run build` is the whole frontend gate: vue-tsc -b, then biome check,
REM  then vite build (see web/package.json). Biome runs `check`, never
REM  `check --write`, so a build cannot rewrite source files. A non-zero exit
REM  fails the build.
echo.
echo [1/3] Building frontend ...
cd /d "%WEB%"
if errorlevel 1 (
    echo [ERROR] cannot enter "%WEB%".
    exit /b 1
)

if not exist "node_modules" (
    echo       node_modules missing, running npm ci ...
    call npm ci
    if errorlevel 1 (
        echo [ERROR] npm ci failed.
        exit /b 1
    )
)

call npm run build
if errorlevel 1 (
    echo [ERROR] frontend build failed.
    exit /b 1
)

if not exist "%WEB%\dist\index.html" (
    echo [ERROR] web\dist\index.html missing after the build.
    exit /b 1
)
echo       frontend OK - web\dist is ready

REM ----------------------------------------------------------------- backend --
echo.
echo [2/3] Building backend ^(embeds web\dist^) ...
cd /d "%ROOT%"
if errorlevel 1 (
    echo [ERROR] cannot return to "%ROOT%".
    exit /b 1
)

cargo build --release --locked -p ninerustrouter
if errorlevel 1 (
    echo [ERROR] cargo build failed.
    exit /b 1
)

REM The cargo config may or may not pin a target triple; resolve either layout.
set "BIN=%ROOT%\target\x86_64-pc-windows-msvc\release\ninerustrouter.exe"
if not exist "%BIN%" set "BIN=%ROOT%\target\release\ninerustrouter.exe"
if not exist "%BIN%" (
    echo [ERROR] release binary not found under target\.
    exit /b 1
)

REM -------------------------------------------------------------------- copy --
echo.
echo [3/3] Copying the binary to ninerustrouter-binary\ ...
if not exist "%OUT%" mkdir "%OUT%"
if errorlevel 1 (
    echo [ERROR] cannot create "%OUT%".
    exit /b 1
)

copy /y "%BIN%" "%OUT%\ninerustrouter.exe" >nul
if errorlevel 1 (
    echo [ERROR] copy failed.
    tasklist /FI "IMAGENAME eq ninerustrouter.exe" /NH 2>nul | "%SystemRoot%\System32\findstr.exe" /I "ninerustrouter.exe" >nul
    if not errorlevel 1 echo         ninerustrouter.exe is running and holds the file - stop it first ^(ninerustrouter stop^).
    exit /b 1
)

echo.
echo ============================================================
echo  Build complete
echo    binary: %OUT%\ninerustrouter.exe
for %%A in ("%OUT%\ninerustrouter.exe") do echo    size:   %%~zA bytes
echo ============================================================

"%SystemRoot%\System32\WindowsPowerShell\v1.0\powershell.exe" -NoProfile -Command ^
    "$v = (Get-Content -Raw '%ROOT%\PROJECT_VERSION').Trim();" ^
    "$name = 'ninerustrouter-windows-%ARCH%-' + $v + '.zip';" ^
    "$zip = Join-Path '%OUT%' $name;" ^
    "Remove-Item $zip -ErrorAction SilentlyContinue;" ^
    "Compress-Archive -Path (Join-Path '%OUT%' 'ninerustrouter.exe') -DestinationPath $zip -Force;" ^
    "Write-Host ('   asset:  ' + $zip)"
if errorlevel 1 (
    echo [WARN] could not create the release zip - the plain binary is still in "%OUT%".
) else (
    echo    the zip holds the bare ninerustrouter.exe ^(no version in the name^).
)

endlocal
exit /b 0
