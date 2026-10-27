$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$source = Join-Path $root 'target/depotkit-source'
$build = Join-Path $root 'target/depotkit-build'
$output = Join-Path $root 'target/depotkit-runtime'
$testBuild = Join-Path $root 'target/depotkit-tests'

if (-not (Test-Path "$source/.git")) {
    git clone --no-checkout https://github.com/Neo-AzTeK/depotkit.git $source
    if ($LASTEXITCODE -ne 0) { throw 'Cannot fetch depotkit' }
}
git -C $source checkout --detach 976f3b8fa50d09bea6e2c1c871827752e32cbb91
if ($LASTEXITCODE -ne 0) { throw 'Cannot check out pinned depotkit revision' }
# This checkout is generated under target; rebuild patches from the pinned source.
git -C $source reset --hard 976f3b8fa50d09bea6e2c1c871827752e32cbb91
if ($LASTEXITCODE -ne 0) { throw 'Cannot reset generated depotkit source' }
foreach ($patchName in @('depotkit-utf8.patch', 'depotkit-resume.patch')) {
    $patch = Join-Path $PSScriptRoot $patchName
    git -C $source apply --check $patch
    if ($LASTEXITCODE -ne 0) { throw "Cannot verify $patchName" }
    git -C $source apply $patch
    if ($LASTEXITCODE -ne 0) { throw "Cannot apply $patchName" }
}

$options = @('-A', 'x64', '-DZSTD_BUILD_SHARED=OFF', '-DZSTD_BUILD_STATIC=ON', '-DCMAKE_DISABLE_FIND_PACKAGE_ZLIB=ON', '-DCMAKE_DISABLE_FIND_PACKAGE_zstd=ON', '-DCMAKE_MSVC_RUNTIME_LIBRARY=MultiThreaded')
cmake -S $source -B $testBuild @options -DDEPOTKIT_BUILD_SHARED=OFF -DDEPOTKIT_BUILD_TESTS=ON
if ($LASTEXITCODE -ne 0) { throw 'Cannot configure depotkit tests' }
cmake --build $testBuild --config Release
if ($LASTEXITCODE -ne 0) { throw 'Cannot build depotkit tests' }
ctest --test-dir $testBuild -C Release --output-on-failure
if ($LASTEXITCODE -ne 0) { throw 'Depotkit tests failed' }

cmake -S $source -B $build @options -DDEPOTKIT_BUILD_SHARED=ON -DDEPOTKIT_BUILD_TESTS=OFF
if ($LASTEXITCODE -ne 0) { throw 'Cannot configure depotkit' }
cmake --build $build --config Release
if ($LASTEXITCODE -ne 0) { throw 'Cannot build depotkit' }

New-Item -ItemType Directory -Force -Path $output | Out-Null
Copy-Item "$build/Release/depotkit.dll" $output
Copy-Item "$source/LICENSE" "$output/depotkit-LICENSE.txt"
Copy-Item "$source/NOTICE" "$output/depotkit-NOTICE.txt"
Copy-Item "$build/_deps/zlib-src/LICENSE" "$output/zlib-LICENSE.txt"
Copy-Item "$build/_deps/zstd-src/LICENSE" "$output/zstd-LICENSE.txt"

$config = @{ bundle = @{ resources = @{
    '../target/depotkit-runtime/depotkit.dll' = 'depotkit.dll'
    '../target/depotkit-runtime/depotkit-LICENSE.txt' = 'depotkit-LICENSE.txt'
    '../target/depotkit-runtime/depotkit-NOTICE.txt' = 'depotkit-NOTICE.txt'
    '../target/depotkit-runtime/zlib-LICENSE.txt' = 'zlib-LICENSE.txt'
    '../target/depotkit-runtime/zstd-LICENSE.txt' = 'zstd-LICENSE.txt'
} } }
$config | ConvertTo-Json -Depth 5 | Set-Content "$root/src-tauri/tauri.windows.conf.json"
