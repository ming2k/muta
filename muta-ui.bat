@echo off
rem muta-ui.bat — one-click launcher for Muta Web UI on Windows
setlocal enabledelayedexpansion

rem Check if muta.exe exists in current directory or PATH
where muta >nul 2>nul
if %ERRORLEVEL% neq 0 (
    if exist "%~dp0muta.exe" (
        set "PATH=%~dp0;%PATH%"
    ) else if exist "%LOCALAPPDATA%\Programs\muta\bin\muta.exe" (
        set "PATH=%LOCALAPPDATA%\Programs\muta\bin;%PATH%"
    ) else (
        echo Error: muta.exe not found. Please install Muta.
        pause
        exit /b 1
    )
)

rem 1. Check if daemon is running; if not, start it
muta status >nul 2>nul
if %ERRORLEVEL% neq 0 (
    echo Starting Muta daemon in background...
    muta start
    timeout /t 1 /nobreak >nul
)

rem 2. Get local token
for /f "tokens=*" %%i in ('muta token 2^>nul') do set "TOKEN=%%i"

set "URL=http://127.0.0.1:9800"
if not "%TOKEN%"=="" (
    set "URL=http://127.0.0.1:9800/?token=%TOKEN%"
)

echo Opening Muta Web UI at %URL%...
start "" "%URL%"
