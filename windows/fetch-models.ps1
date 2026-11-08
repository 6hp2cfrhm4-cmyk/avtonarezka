# Скачивает локальные модели для анализа и проверяет контрольные суммы (Windows, PowerShell 5.1+).
# Кладёт их туда, где их ищет приложение: %LOCALAPPDATA%\app.avtonarezka.desktop\models
# Общий размер около 3 ГБ. Скачивание можно прервать и запустить снова: оно продолжится.
#
# Запуск (правый клик по файлу -> «Выполнить с помощью PowerShell») или из терминала:
#   powershell -ExecutionPolicy Bypass -File .\fetch-models.ps1
# Другая папка: .\fetch-models.ps1 -Root D:\models
param(
    [string]$Root = (Join-Path $env:LOCALAPPDATA 'app.avtonarezka.desktop\models')
)

$ErrorActionPreference = 'Stop'

function Get-Model {
    param([string]$Dir, [string]$Url, [string]$File, [long]$Size, [string]$Sha)

    $folder = Join-Path $Root $Dir
    $target = Join-Path $folder $File
    $part = "$target.part"
    New-Item -ItemType Directory -Force -Path $folder | Out-Null

    if ((Test-Path $target) -and ((Get-Item $target).Length -eq $Size)) {
        Write-Host "[$Dir] $File уже скачан"
        return $true
    }

    Write-Host ("[{0}] скачиваю {1} ({2} МБ)" -f $Dir, $File, [math]::Floor($Size / 1000000))
    # curl.exe есть в Windows 10 1803+ и 11: умеет докачку (-C -) и повторы, в отличие от Invoke-WebRequest
    $curl = Join-Path $env:SystemRoot 'System32\curl.exe'
    if (-not (Test-Path $curl)) { Write-Host "[$Dir] ОШИБКА: не найден curl.exe"; return $false }
    & $curl -L --fail --retry 4 --retry-delay 3 -C - -sS -o $part $Url
    if ($LASTEXITCODE -ne 0) { Write-Host "[$Dir] ОШИБКА загрузки (код $LASTEXITCODE)"; return $false }

    $got = (Get-Item $part).Length
    if ($got -ne $Size) { Write-Host "[$Dir] ОШИБКА: размер $got вместо $Size"; return $false }
    $real = (Get-FileHash -Algorithm SHA256 $part).Hash.ToLower()
    if ($real -ne $Sha) {
        Write-Host "[$Dir] ОШИБКА: контрольная сумма $real вместо $Sha"
        Remove-Item -Force $part
        return $false
    }
    Move-Item -Force $part $target
    Write-Host "[$Dir] готово"
    return $true
}

$ok = $true

# Whisper large-v3-turbo (распознавание речи), квантование q5_0
$ok = (Get-Model 'whisper-large-v3-turbo-q5_0' `
    'https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo-q5_0.bin' `
    'ggml-large-v3-turbo-q5_0.bin' 574041195 '394221709cd5ad1f40c46e6031ca61bce88931e6e088c188294c6d5a55ffa7e2') -and $ok
# YAMNet (смех, крики, аплодисменты)
$ok = (Get-Model 'yamnet-onnx' `
    'https://huggingface.co/zeropointnine/yamnet-onnx/resolve/main/yamnet.onnx' `
    'yamnet.onnx' 16093603 '1510041dce24a2e9e84ec546807ac408ae496da6d1ed41bc3ccba649623f8e19') -and $ok
$ok = (Get-Model 'yamnet-onnx' `
    'https://huggingface.co/zeropointnine/yamnet-onnx/resolve/main/yamnet_class_map.csv' `
    'yamnet_class_map.csv' 14096 'cdf24d193e196d9e95912a2667051ae203e92a2ba09449218ccb40ef787c6df2') -and $ok
# Qwen3-4B (заголовки, подписи, оценка моментов), квантование Q4_K_M
$ok = (Get-Model 'qwen3-4b-q4_k_m' `
    'https://huggingface.co/Qwen/Qwen3-4B-GGUF/resolve/main/Qwen3-4B-Q4_K_M.gguf' `
    'Qwen3-4B-Q4_K_M.gguf' 2497280256 '7485fe6f11af29433bc51cab58009521f205840f5b4ae3a32fa7f92e8534fdf5') -and $ok

if ($ok) {
    Write-Host "Все модели на месте: $Root"
    exit 0
}
Write-Host 'Часть моделей не скачана, запусти скрипт ещё раз.'
exit 1
