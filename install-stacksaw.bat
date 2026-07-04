@echo off
rem install-stacksaw.bat - build and install the `stacksaw` binary locally from
rem a synced source tree (Windows). Run it from anywhere:
rem
rem     install-stacksaw.bat
rem
rem It installs into Cargo's bin directory (usually %USERPROFILE%\.cargo\bin);
rem make sure that is on your PATH. Re-running rebuilds and replaces the install.

setlocal

rem Repository root is the directory containing this script.
set "SCRIPT_DIR=%~dp0"
pushd "%SCRIPT_DIR%"

where cargo >nul 2>nul
if errorlevel 1 (
    echo error: 'cargo' was not found on PATH.
    echo Install the Rust toolchain from https://rustup.rs and try again.
    popd
    endlocal
    exit /b 1
)

echo Building and installing stacksaw from: %SCRIPT_DIR%
rem --locked pins the exact dependency versions from Cargo.lock; --force rebuilds
rem and replaces any existing install so a fresh sync always takes effect.
cargo install --path crates\stacksaw --locked --force
if errorlevel 1 (
    echo.
    echo error: cargo install failed.
    popd
    endlocal
    exit /b 1
)

echo.
if defined CARGO_HOME (
    echo Installed 'stacksaw' to %CARGO_HOME%\bin.
) else (
    echo Installed 'stacksaw' to %USERPROFILE%\.cargo\bin.
)

popd
endlocal
