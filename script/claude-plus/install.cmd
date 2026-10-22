@echo off
setlocal
set "REPO=Shawdown/zed-claude-code-plus"
set "DOWNLOAD_DIR=%TEMP%\zed-claude-code-plus"

where gh >nul 2>nul || (echo The GitHub CLI is required: https://cli.github.com & goto :fail)
gh auth status >nul 2>nul || gh auth login || goto :fail

if exist "%DOWNLOAD_DIR%" rmdir /s /q "%DOWNLOAD_DIR%"
mkdir "%DOWNLOAD_DIR%" || goto :fail

echo Downloading the latest release of %REPO%...
gh release download --repo %REPO% --pattern "*.exe" --dir "%DOWNLOAD_DIR%" || goto :fail

set "INSTALLER="
for %%F in ("%DOWNLOAD_DIR%\*.exe") do set "INSTALLER=%%F"
if not defined INSTALLER (echo No installer found in the latest release. & goto :fail)

echo Installing %INSTALLER%...
echo Close Zed first if it is running.
start "" /wait "%INSTALLER%" /SILENT /CLOSEAPPLICATIONS || goto :fail
rmdir /s /q "%DOWNLOAD_DIR%"

echo.
echo Done. You can start Zed now.
pause
exit /b 0

:fail
echo.
echo Installation failed.
pause
exit /b 1
