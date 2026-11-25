param([switch]$SkipBuild, [switch]$PublicAsset, [string]$Executable, [string]$OutputDirectory)
$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path $PSScriptRoot -Parent
Push-Location $projectRoot
$oldFlags = $env:CARGO_ENCODED_RUSTFLAGS
$oldCFlags = $env:CFLAGS_x86_64_pc_windows_msvc
$oldEscapedFlags = $env:CC_SHELL_ESCAPED_FLAGS
try {
    $version = [regex]::Match((Get-Content Cargo.toml -Raw), '(?m)^version = "([^"]+)"').Groups[1].Value
    if ($version -notmatch '^\d+\.\d+\.\d+$') { throw 'Leidao 正式版需要三段数字版本号。' }
    if (!$SkipBuild) {
        $cargoHome = if ($env:CARGO_HOME) {$env:CARGO_HOME} else {Join-Path $env:USERPROFILE '.cargo'}
        $env:CARGO_ENCODED_RUSTFLAGS = @('-C','target-feature=+crt-static',"--remap-path-prefix=$cargoHome=cargo", "--remap-path-prefix=$projectRoot=leidao",'-C','link-arg=/PDBALTPATH:%_PDB%') -join [char]0x1f
        # QuickJS sets CFLAGS itself. Pass the trim option through cc's target-specific flags.
        $trimFlag = '/d1trimfile:' + $projectRoot
        $quotedTrimFlag = "'" + $trimFlag.Replace("'", "'\''") + "'"
        $env:CC_SHELL_ESCAPED_FLAGS = '1'
        $env:CFLAGS_x86_64_pc_windows_msvc = "$oldCFlags $quotedTrimFlag".Trim()
        cargo build --locked --release --bin daozai
        if ($LASTEXITCODE -ne 0) { throw 'Leidao 构建失败。' }
    }
    if (!$Executable) { $Executable = Join-Path $projectRoot 'target/release/daozai.exe' }
    $binary = Get-Item -LiteralPath $Executable
    & (Join-Path $PSScriptRoot 'check-runtime.ps1') -Executable $binary.FullName
    if (($binary.VersionInfo.FileVersion -ne $version -and $binary.VersionInfo.FileVersion -ne "$version.0") -or $binary.VersionInfo.ProductName -ne 'Leidao') { throw '程序版本或产品不匹配。' }
    if (!$SkipBuild) {
        $bytes = [IO.File]::ReadAllBytes($binary.FullName)
        $privatePaths = @($env:USERPROFILE, $projectRoot) | Where-Object { $_ } | ForEach-Object {
            $_.Replace('/','\')
            $_.Replace('\','/')
        }
        $contents = @(
            [Text.Encoding]::UTF8.GetString($bytes)
            [Text.Encoding]::Unicode.GetString($bytes)
            [Text.Encoding]::Unicode.GetString($bytes, 1, $bytes.Length - 1)
        )
        foreach ($content in $contents) {
            foreach ($path in $privatePaths) {
                if ($content.IndexOf($path,[StringComparison]::OrdinalIgnoreCase) -ge 0) {
                    throw '发布程序包含本机构建路径。'
                }
            }
        }
    }
    if (!$OutputDirectory) { $OutputDirectory = Join-Path $projectRoot 'dist' }
    $output = [IO.Path]::GetFullPath($OutputDirectory)
    $name = "Leidao-v$version-windows-x64"
    $stage = Join-Path $output $name
    # Keep the ZIP's inner directory and exe names compatible with installed updaters.
    $archiveName = if ($PublicAsset) {"FloatingIsland-v$version-windows-x64.zip"} else {"$name.zip"}
    $archive = Join-Path $output $archiveName
    if ((Test-Path -LiteralPath $stage) -or (Test-Path -LiteralPath $archive)) { throw '发布包已存在，请使用新的输出目录。' }
    New-Item -ItemType Directory -Path $stage -Force | Out-Null
    foreach ($exe in @('Leidao.exe','daozai.exe')) { Copy-Item -LiteralPath $binary.FullName -Destination (Join-Path $stage $exe) }
    foreach ($file in @('LICENSE','LICENSE-FLOATING-ISLAND.md','THIRD_PARTY_NOTICES.txt')) { Copy-Item -LiteralPath (Join-Path $projectRoot $file) -Destination $stage }
    $upstreamLicense = Join-Path $projectRoot 'LICENSE-MIT.txt'
    if (Test-Path -LiteralPath $upstreamLicense -PathType Leaf) {
        Copy-Item -LiteralPath $upstreamLicense -Destination $stage
    }
    Copy-Item -LiteralPath (Join-Path $projectRoot 'assets/fonts/OFL-NotoSansSC.txt') -Destination $stage
    Compress-Archive -LiteralPath $stage -DestinationPath $archive
    $hash = (Get-FileHash -LiteralPath $archive).Hash.ToLowerInvariant()
    "$hash  $archiveName" | Set-Content -LiteralPath (Join-Path $output 'SHA256SUMS.txt') -Encoding ascii
    Write-Output $archive
} finally {
    $env:CARGO_ENCODED_RUSTFLAGS = $oldFlags
    $env:CFLAGS_x86_64_pc_windows_msvc = $oldCFlags
    $env:CC_SHELL_ESCAPED_FLAGS = $oldEscapedFlags
    Pop-Location
}
