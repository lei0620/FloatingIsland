param([Parameter(Mandatory=$true)][string]$WorkPath, [Parameter(Mandatory=$true)][string]$TargetExe, [int]$ParentProcessId)
$ErrorActionPreference = 'Stop'
$env:PSModulePath = [IO.Path]::Combine($PSHOME, 'Modules')
$cacheRoot = [IO.Path]::GetFullPath((Join-Path $env:LOCALAPPDATA 'Leidao\Updates')).TrimEnd('\')
$workRoot = [IO.Path]::GetFullPath($WorkPath).TrimEnd('\')
$target = [IO.Path]::GetFullPath($TargetExe)
$targetDir = Split-Path $target -Parent
$staged = Join-Path $targetDir ('leidao-update-' + [Guid]::NewGuid().ToString('N') + '.exe')
$aliasStage = $staged + '.alias'
$backups = @{}
$replaced = @()
$exited = $false
function Assert-PlainPath([string]$PathToCheck) {
    $ancestor = [IO.Path]::GetFullPath($PathToCheck)
    while ($ancestor) {
        if ((Test-Path -LiteralPath $ancestor) -and ((Get-Item -LiteralPath $ancestor -Force).Attributes -band [IO.FileAttributes]::ReparsePoint)) { throw '更新目录不能包含目录链接。' }
        $ancestor = Split-Path $ancestor -Parent
    }
}
try {
    if (-not $workRoot.StartsWith($cacheRoot + '\', [StringComparison]::OrdinalIgnoreCase)) { throw '更新缓存目录无效。' }
    Assert-PlainPath $workRoot
    Assert-PlainPath $target
    if ([IO.Path]::GetFileName($target) -notin @('Leidao.exe','daozai.exe') -or -not (Test-Path -LiteralPath $target -PathType Leaf)) { throw '找不到当前 Leidao 程序。' }
    $version = (Get-Content -LiteralPath (Join-Path $workRoot 'version.txt') -Raw).Trim()
    if ($version -notmatch '^\d+\.\d+\.\d+$') { throw '更新版本号无效。' }
    $archive = Join-Path $workRoot 'update.zip'
    $expectedHash = (Get-Content -LiteralPath (Join-Path $workRoot 'hash.txt') -Raw).Trim()
    if ($expectedHash -notmatch '^[a-fA-F0-9]{64}$' -or (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash -ne $expectedHash) { throw '更新包校验失败。' }
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $zip = [IO.Compression.ZipFile]::OpenRead($archive)
    try {
        $prefix = 'Leidao-v' + $version + '-windows-x64/'
        foreach ($name in @('Leidao.exe','daozai.exe')) {
            $entries = @($zip.Entries | Where-Object {$_.FullName.Replace('\','/') -ceq ($prefix + $name)})
            if ($entries.Count -ne 1 -or $entries[0].Length -le 0 -or $entries[0].Length -gt 200MB) { throw '更新包中缺少唯一的 Leidao 程序。' }
            $destination = if ($name -eq 'Leidao.exe') {$staged} else {$aliasStage}
            [IO.Compression.ZipFileExtensions]::ExtractToFile($entries[0], $destination)
        }
    } finally { $zip.Dispose() }
    if ((Get-FileHash -LiteralPath $staged).Hash -ne (Get-FileHash -LiteralPath $aliasStage).Hash) { throw '更新包的两个程序入口不一致。' }
    $info = [Diagnostics.FileVersionInfo]::GetVersionInfo($staged)
    if (($info.FileVersion -ne $version -and $info.FileVersion -ne "$version.0") -or $info.ProductName -ne 'Leidao') { throw '安装包版本或产品不匹配。' }
    $current = [Diagnostics.FileVersionInfo]::GetVersionInfo($target).FileVersion
    if ($current -match '^\d+\.\d+\.\d+(\.\d+)?$' -and [version]$version -le [version]$current) { throw '更新版本必须高于当前版本。' }
    $targets = @($target)
    $otherName = if ([IO.Path]::GetFileName($target) -ieq 'Leidao.exe') {'daozai.exe'} else {'Leidao.exe'}
    $other = Join-Path $targetDir $otherName
    if (Test-Path -LiteralPath $other -PathType Leaf) {
        Assert-PlainPath $other
        if ((Get-FileHash -LiteralPath $other).Hash -ne (Get-FileHash -LiteralPath $target).Hash) { throw '兼容入口与当前程序不同，请先修复本机安装。' }
        $targets += $other
    }
    # Keep executable backups outside the disposable cache. Never rewrite user data.
    $backupDir = Join-Path $targetDir ('备份\' + (Get-Date -Format 'yyyy-MM-dd_HHmmss') + '_Leidao更新_' + [Guid]::NewGuid().ToString('N'))
    Assert-PlainPath $backupDir
    New-Item -ItemType Directory -Path $backupDir -Force | Out-Null
    foreach ($path in $targets) {
        $backup = Join-Path $backupDir ([IO.Path]::GetFileName($path))
        [IO.File]::Copy($path, $backup, $false)
        $backups[$path] = $backup
    }
    Set-Content -LiteralPath (Join-Path $workRoot 'ready') -Value 'ok'
    if ($ParentProcessId -gt 0) {
        $parent = Get-Process -Id $ParentProcessId -ErrorAction SilentlyContinue
        if ($parent -and -not $parent.WaitForExit(60000)) { throw 'Leidao 尚未退出，请稍后重试。' }
    }
    $exited = $true
    foreach ($path in $targets) {
        $source = if ($path -eq $target) {$staged} else {$aliasStage}
        [IO.File]::Replace($source, $path, [NullString]::Value)
        $replaced += $path
    }
    if (Test-Path -LiteralPath $aliasStage) { Remove-Item -LiteralPath $aliasStage -Force }
    Start-Process -FilePath $target -WorkingDirectory $targetDir -WindowStyle Hidden
    # No registry, startup, permissions, user-data or data-path-marker changes.
    if ($workRoot.StartsWith($cacheRoot + '\', [StringComparison]::OrdinalIgnoreCase)) {
        try { Assert-PlainPath $workRoot; Remove-Item -LiteralPath $workRoot -Recurse -Force } catch { }
    }
} catch {
    $message = $_.Exception.Message
    $restored = $true
    foreach ($path in $replaced) {
        try { [IO.File]::Copy($backups[$path], $path, $true) }
        catch { $restored = $false }
    }
    foreach ($temporary in @($staged,$aliasStage)) { if (Test-Path -LiteralPath $temporary) { Remove-Item -LiteralPath $temporary -Force } }
    if ($workRoot.StartsWith($cacheRoot + '\', [StringComparison]::OrdinalIgnoreCase)) {
        [IO.File]::WriteAllText((Join-Path $workRoot 'error.txt'), $message)
    }
    if ($exited -and $restored) { Start-Process -FilePath $target -WorkingDirectory $targetDir -WindowStyle Hidden }
    exit 1
}
