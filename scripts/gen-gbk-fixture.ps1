# 生成 GBK 编码夹具（取 chinese_message.log 前 6 行转存 GBK）
$ErrorActionPreference = 'Stop'
$c = Get-Content -Raw -Encoding UTF8 "$PSScriptRoot\..\fixtures\chinese_message.log"
$gbk = [System.Text.Encoding]::GetEncoding(936)
$lines = ($c -split "`r?`n") | Where-Object { $_ -ne '' } | Select-Object -First 6
$out = ($lines -join "`r`n") + "`r`n"
[System.IO.File]::WriteAllText("$PSScriptRoot\..\fixtures\gbk_encoded.log", $out, $gbk)
Write-Host ("gbk written: " + (Get-Item "$PSScriptRoot\..\fixtures\gbk_encoded.log").Length + " bytes")
