# 使用独立目录和最小测试程序验证更新，不启动或替换日常使用的岛仔。
param([switch]$EncryptedCache)
$ErrorActionPreference = 'Stop'
$project = Split-Path $PSScriptRoot -Parent
$testRoot = Join-Path $project ('target\update-test-' + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $testRoot | Out-Null
$oldLocal = $env:LOCALAPPDATA
try {
    $env:LOCALAPPDATA = if ($EncryptedCache) { Join-Path $oldLocal ('DaozaiUpdateTest-' + [Guid]::NewGuid().ToString('N')) } else { Join-Path $testRoot 'local' }
    New-Item -ItemType Directory -Path $env:LOCALAPPDATA -Force | Out-Null
    if ($EncryptedCache -and -not ((Get-Item -LiteralPath $env:LOCALAPPDATA).Attributes -band [IO.FileAttributes]::Encrypted)) { throw '测试缓存目录必须启用 EFS 加密' }
    $compiler = Join-Path $env:WINDIR 'Microsoft.NET\Framework64\v4.0.30319\csc.exe'
    $source = Join-Path $testRoot 'fixture.cs'
    '[assembly:System.Reflection.AssemblyFileVersion("9.8.7")][assembly:System.Reflection.AssemblyProduct("Leidao")] class Fixture { static void Main() { var root=System.AppDomain.CurrentDomain.BaseDirectory; System.IO.File.WriteAllText(System.IO.Path.Combine(root,"fixture.pid"),System.Diagnostics.Process.GetCurrentProcess().Id.ToString()); System.IO.File.WriteAllText(System.IO.Path.Combine(root,"started.txt"),"ok"); } }' | Set-Content -LiteralPath $source
    $fixture = Join-Path $testRoot 'fixture.exe'
    & $compiler /nologo /target:winexe "/out:$fixture" $source
    if ($LASTEXITCODE -ne 0) { throw '测试程序编译失败' }
    $oldSource = Join-Path $testRoot 'old.cs'
    (Get-Content -LiteralPath $source -Raw).Replace('9.8.7','9.8.6') | Set-Content -LiteralPath $oldSource
    $oldFixture = Join-Path $testRoot 'old.exe'
    & $compiler /nologo /target:winexe "/out:$oldFixture" $oldSource
    if($LASTEXITCODE -ne 0){throw '旧版测试程序编译失败'}
    $foreignSource=Join-Path $testRoot 'foreign.cs'
    (Get-Content -LiteralPath $source -Raw).Replace('AssemblyProduct("Leidao")','AssemblyProduct("Daozai")') | Set-Content -LiteralPath $foreignSource
    $foreignFixture=Join-Path $testRoot 'foreign.exe'
    & $compiler /nologo /target:winexe "/out:$foreignFixture" $foreignSource
    if($LASTEXITCODE -ne 0){throw '产品标识测试程序编译失败'}
    $installer = Join-Path $testRoot 'apply.ps1'
    [IO.File]::WriteAllText($installer, [IO.File]::ReadAllText((Join-Path $project 'installer\update.ps1')), [Text.UTF8Encoding]::new($true))
    $targetDir = Join-Path $testRoot 'installed'
    New-Item -ItemType Directory -Path $targetDir | Out-Null
    $target = Join-Path $targetDir 'Leidao.exe'
    $alias = Join-Path $targetDir 'daozai.exe'
    $marker = Join-Path $targetDir 'daozai-data-dir.txt'
    Set-Content -LiteralPath $marker -Value 'preserve-custom-data-path'
    $data = Join-Path $targetDir 'settings.json'
    Set-Content -LiteralPath $data -Value 'keep-user-data'
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    foreach ($case in @('success','inherited-shell','single-entry','bad-hash','wrong-version','traversal','upstream','duplicate','unequal-entry','foreign-alias','rollback-lock','downgrade','wrong-product')) {
        Remove-Item -LiteralPath (Join-Path $targetDir 'started.txt') -ErrorAction SilentlyContinue
        Remove-Item -LiteralPath (Join-Path $targetDir 'fixture.pid') -ErrorAction SilentlyContinue
        Copy-Item -LiteralPath $oldFixture -Destination $target -Force
        if($case -eq 'downgrade'){Copy-Item -LiteralPath $fixture -Destination $target -Force}
        if($case -eq 'single-entry'){Remove-Item -LiteralPath $alias -ErrorAction SilentlyContinue}else{Copy-Item -LiteralPath $target -Destination $alias -Force}
        if($case -eq 'foreign-alias'){Set-Content -LiteralPath $alias -Value 'other-program'}
        $before = (Get-FileHash -LiteralPath $target).Hash
        $aliasBefore=if(Test-Path -LiteralPath $alias){(Get-FileHash -LiteralPath $alias).Hash}else{''}
        $work = Join-Path $env:LOCALAPPDATA ('Leidao\Updates\' + $case)
        New-Item -ItemType Directory -Path $work -Force | Out-Null
        $archive = Join-Path $work 'update.zip'
        $zip = [IO.Compression.ZipFile]::Open($archive, [IO.Compression.ZipArchiveMode]::Create)
        $version = if ($case -eq 'wrong-version') {'9.8.8'} else {'9.8.7'}
        $entry = if ($case -eq 'traversal') {'../daozai.exe'} elseif($case -eq 'upstream'){"Daozai-v$version-windows-x64/daozai.exe"}else {"Leidao-v$version-windows-x64/Leidao.exe"}
        [IO.Compression.ZipFileExtensions]::CreateEntryFromFile($zip, $(if($case -eq "wrong-product"){$foreignFixture}else{$fixture}), $entry) | Out-Null
        [IO.Compression.ZipFileExtensions]::CreateEntryFromFile($zip, $(if($case -eq "unequal-entry"){$source}elseif($case -eq "wrong-product"){$foreignFixture}else{$fixture}), "Leidao-v$version-windows-x64/daozai.exe") | Out-Null
        if($case -eq 'duplicate'){[IO.Compression.ZipFileExtensions]::CreateEntryFromFile($zip, $(if($case -eq "wrong-product"){$foreignFixture}else{$fixture}), $entry) | Out-Null}
        $zip.Dispose()
        Set-Content -LiteralPath (Join-Path $work 'version.txt') -Value $version
        $hash = if ($case -eq 'bad-hash') {'a' * 64} else {(Get-FileHash -LiteralPath $archive).Hash}
        Set-Content -LiteralPath (Join-Path $work 'hash.txt') -Value $hash
        $shell = Join-Path $env:WINDIR 'System32\WindowsPowerShell\v1.0\powershell.exe'
        $locked=$null
        if($case -eq 'rollback-lock'){$locked=[IO.File]::Open($alias,[IO.FileMode]::Open,[IO.FileAccess]::Read,[IO.FileShare]::Read)}
        if ($case -eq 'inherited-shell') {
            # 模拟应用继承 PowerShell 7 的模块路径，绕过交互式 shell 对子进程环境的自动修正。
            $start = [Diagnostics.ProcessStartInfo]::new($shell)
            $start.UseShellExecute = $false
            $start.CreateNoWindow = $true
            $start.EnvironmentVariables['PSModulePath'] = $env:PSModulePath
            $start.Arguments = "-NoProfile -NonInteractive -ExecutionPolicy Bypass -File `"$installer`" -WorkPath `"$work`" -TargetExe `"$target`" -ParentProcessId 0"
            $process = [Diagnostics.Process]::Start($start)
            if (-not $process.WaitForExit(30000)) { throw '继承终端环境的更新测试超时' }
            $code = $process.ExitCode
            $process.Dispose()
        } else {
            & $shell -NoProfile -NonInteractive -ExecutionPolicy Bypass -File $installer -WorkPath $work -TargetExe $target -ParentProcessId 0
            $code = $LASTEXITCODE
        }
        if($locked){$locked.Dispose()}
        if ($case -in @('success','inherited-shell','single-entry')) {
            if ($code -ne 0) { throw "更新安装失败：$case" }
            if ((Get-FileHash -LiteralPath $target).Hash -ne (Get-FileHash -LiteralPath $fixture).Hash) { throw '未替换为新版程序' }
            if($case -ne 'single-entry' -and (Get-FileHash -LiteralPath $alias).Hash -ne (Get-FileHash -LiteralPath $fixture).Hash){throw '兼容入口未同步'}
            $backups=@(Get-ChildItem (Join-Path $targetDir '备份') -Filter Leidao.exe -File -Recurse | Where-Object {(Get-FileHash -LiteralPath $_.FullName).Hash -eq $before})
            if(!$backups.Count){throw '未保留旧程序备份'}
            for ($attempt=0; $attempt -lt 50 -and -not (Test-Path -LiteralPath (Join-Path $targetDir 'started.txt')); $attempt++) { Start-Sleep -Milliseconds 100 }
            if (-not (Test-Path -LiteralPath (Join-Path $targetDir 'started.txt'))) { throw '更新后没有重启' }
        } else {
            if ($code -eq 0 -or -not (Test-Path -LiteralPath (Join-Path $work 'error.txt'))) { throw "未拒绝无效更新：$case" }
            if ((Get-FileHash -LiteralPath $target).Hash -ne $before) { throw '无效更新修改了原程序' }
            if($aliasBefore -and (Get-FileHash -LiteralPath $alias).Hash -ne $aliasBefore){throw '失败后兼容入口未保留或恢复'}
        }
        if((Get-Content -LiteralPath $marker -Raw).Trim() -ne 'preserve-custom-data-path'){throw '数据路径标记被修改'}
        if ((Get-Content -LiteralPath $data -Raw).Trim() -ne 'keep-user-data') { throw '用户数据被修改' }
        # The start marker is written before the CLR releases the executable file.
        # Await only this fixture process before the next case overwrites the target.
        $fixturePidFile = Join-Path $targetDir 'fixture.pid'
        if ($case -in @('success','inherited-shell','single-entry','rollback-lock')) {
            for ($attempt = 0; $attempt -lt 100 -and -not (Test-Path -LiteralPath $fixturePidFile); $attempt++) { Start-Sleep -Milliseconds 50 }
            if (-not (Test-Path -LiteralPath $fixturePidFile)) { throw "未找到重启的测试进程：$case" }
        }
        if (Test-Path -LiteralPath $fixturePidFile) {
            $fixtureProcessId = [int]([IO.File]::ReadAllText($fixturePidFile).Trim())
            $fixtureProcess = Get-Process -Id $fixtureProcessId -ErrorAction SilentlyContinue
            if ($fixtureProcess -and $fixtureProcess.Path -eq $target -and -not $fixtureProcess.WaitForExit(10000)) {
                throw "测试程序未按时退出：$case"
            }
        }
        Write-Output "通过：$case"
    }
} finally {
    $env:LOCALAPPDATA = $oldLocal
}
# Rejected fixture updates intentionally return 1; the test suite itself succeeded.
exit 0
