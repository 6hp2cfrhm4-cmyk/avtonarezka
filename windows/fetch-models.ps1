﻿# Скачивает модели для Автонарезки. Уже скачанные файлы пропускает, прерванные перекачивает.
# Запуск: powershell -ExecutionPolicy Bypass -File .\fetch-models.ps1
$ErrorActionPreference = 'Stop'
# Windows PowerShell 5.1 по умолчанию может не работать с HTTPS-сайтами без TLS 1.2
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
# без этого Invoke-WebRequest в Windows PowerShell качает очень медленно
$ProgressPreference = 'SilentlyContinue'

$root = Join-Path $env:LOCALAPPDATA 'app.avtonarezka.desktop\models'

$files = @(
    @{ Url = 'https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo-q5_0.bin'; Path = 'whisper-large-v3-turbo-q5_0\ggml-large-v3-turbo-q5_0.bin'; Size = 574041195 },
    @{ Url = 'https://huggingface.co/zeropointnine/yamnet-onnx/resolve/main/yamnet.onnx'; Path = 'yamnet-onnx\yamnet.onnx'; Size = 16093603 },
    @{ Url = 'https://huggingface.co/zeropointnine/yamnet-onnx/resolve/main/yamnet_class_map.csv'; Path = 'yamnet-onnx\yamnet_class_map.csv'; Size = 14096 },
    @{ Url = 'https://huggingface.co/Qwen/Qwen3-4B-GGUF/resolve/main/Qwen3-4B-Q4_K_M.gguf'; Path = 'qwen3-4b-q4_k_m\Qwen3-4B-Q4_K_M.gguf'; Size = 2497280256 }
)

foreach ($f in $files) {
    $target = Join-Path $root $f.Path
    if ((Test-Path $target) -and (Get-Item $target).Length -eq $f.Size) {
        Write-Host "уже есть: $($f.Path)"
        continue
    }
    New-Item -ItemType Directory -Force -Path (Split-Path $target) | Out-Null
    Write-Host "качаю: $($f.Path) (это может занять время)"
    Invoke-WebRequest -UseBasicParsing -Uri $f.Url -OutFile $target
    if ((Get-Item $target).Length -ne $f.Size) {
        throw "файл скачался не полностью: $($f.Path). Запусти скрипт ещё раз"
    }
    Write-Host "готово: $($f.Path)"
}

Write-Host "Все модели на месте: $root"
