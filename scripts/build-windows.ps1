# 在 Visual Studio 开发者 PowerShell 中构建 Windows x64 便携版。
# 依赖 Rust stable、MSVC C++ 构建工具和 Windows SDK；无需 Zig。
[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent $PSScriptRoot
Push-Location -LiteralPath $repoRoot
try {
    # 静态链接 MSVC 运行库，便携版无需单独附带 VC Runtime DLL。
    $previousRustFlags = $env:RUSTFLAGS
    $env:RUSTFLAGS = if ($previousRustFlags -match 'target-feature=\+crt-static') {
        $previousRustFlags
    } else {
        "$previousRustFlags -C target-feature=+crt-static".Trim()
    }
    try {
        cargo build --release -p ms-app --target x86_64-pc-windows-msvc --locked
        if ($LASTEXITCODE -ne 0) {
            throw "Windows 构建失败（cargo exit code: $LASTEXITCODE）"
        }
    }
    finally {
        $env:RUSTFLAGS = $previousRustFlags
    }

    $outputDirectory = Join-Path $repoRoot 'dist/windows-x64'
    New-Item -ItemType Directory -Path $outputDirectory -Force | Out-Null
    $binary = Join-Path $outputDirectory 'fesilent-mouse.exe'
    Copy-Item -LiteralPath (Join-Path $repoRoot 'target/x86_64-pc-windows-msvc/release/fesilent-mouse.exe') -Destination $binary -Force
    $checksum = (Get-FileHash -LiteralPath $binary -Algorithm SHA256).Hash.ToLowerInvariant()
    "$checksum  fesilent-mouse.exe" | Set-Content -LiteralPath (Join-Path $outputDirectory 'SHA256SUMS') -Encoding ascii
    Write-Output "已生成 $binary"
}
finally {
    Pop-Location
}
