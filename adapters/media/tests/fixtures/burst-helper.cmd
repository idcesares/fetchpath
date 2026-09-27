@echo off
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0burst-helper.ps1" %*
exit /b %ERRORLEVEL%
