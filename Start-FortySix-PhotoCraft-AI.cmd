@echo off
if "%~1"=="" (
    start "" "%~dp0target\release\photocraft-ai-rail.exe" "X:\Projs\FortySix-MiniGame\assets\defold\gui\ui_window_character.gui"
) else (
    start "" "%~dp0target\release\photocraft-ai-rail.exe" %*
)
