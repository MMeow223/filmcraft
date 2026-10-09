@echo off
rem Start FilmCraft's MCP server on stdio: filmcraft-cli mcp [args].
rem Finds filmcraft-cli: %FILMCRAFT_CLI%, then filmcraft-cli on PATH, then
rem target\release\filmcraft-cli.exe in the FilmCraft checkout this script is in (or %FILMCRAFT_REPO%),
rem building it with cargo first if needed. Only stdout carries MCP; messages go to stderr.
setlocal

if defined FILMCRAFT_CLI (
  "%FILMCRAFT_CLI%" mcp %*
  exit /b %ERRORLEVEL%
)

where filmcraft-cli >nul 2>nul
if %ERRORLEVEL%==0 (
  filmcraft-cli mcp %*
  exit /b %ERRORLEVEL%
)

set "REPO=%FILMCRAFT_REPO%"
if not defined REPO set "REPO=%~dp0..\..\.."
if not exist "%REPO%\apps\filmcraft-cli" (
  echo filmcraft-mcp: filmcraft-cli not found: install a FilmCraft release, set FILMCRAFT_CLI to the binary, or set FILMCRAFT_REPO to a FilmCraft checkout 1>&2
  exit /b 1
)

set "TARGET=%CARGO_TARGET_DIR%"
if not defined TARGET set "TARGET=%REPO%\target"
set "CLI=%TARGET%\release\filmcraft-cli.exe"
if not exist "%CLI%" (
  echo filmcraft-mcp: building filmcraft-cli in %REPO% ^(first run only; this takes a few minutes^) 1>&2
  pushd "%REPO%"
  cargo build --release -p filmcraft-cli 1>&2
  if errorlevel 1 (
    popd
    echo filmcraft-mcp: cargo build failed 1>&2
    exit /b 1
  )
  popd
)

"%CLI%" mcp %*
exit /b %ERRORLEVEL%