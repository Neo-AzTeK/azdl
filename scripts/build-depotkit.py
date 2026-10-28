#!/usr/bin/env python3
"""Clean Linux build of the same pinned Qt-free depotkit used on Windows."""
import json
from pathlib import Path
import shutil
import subprocess

ROOT = Path(__file__).resolve().parent.parent
REVISION = '976f3b8fa50d09bea6e2c1c871827752e32cbb91'
SOURCE = ROOT / 'target/depotkit-source'
BUILD = ROOT / 'target/depotkit-build'
OUTPUT = ROOT / 'target/depotkit-runtime'


def run(*args):
    subprocess.run([str(arg) for arg in args], check=True)


def main():
    for folder in (SOURCE, BUILD, OUTPUT):
        if folder.exists():
            shutil.rmtree(folder)
    run('git', 'clone', '--no-checkout', 'https://github.com/Neo-AzTeK/depotkit.git', SOURCE)
    run('git', '-C', SOURCE, 'fetch', '--no-tags', 'origin', REVISION)
    run('git', '-C', SOURCE, 'checkout', '--detach', REVISION)
    for name in ('depotkit-utf8.patch', 'depotkit-resume.patch'):
        patch = ROOT / 'scripts' / name
        run('git', '-C', SOURCE, 'apply', '--check', patch)
        run('git', '-C', SOURCE, 'apply', patch)
    run('cmake', '-S', SOURCE, '-B', BUILD, '-DCMAKE_BUILD_TYPE=Release',
        '-DCMAKE_POSITION_INDEPENDENT_CODE=ON', '-DDEPOTKIT_BUILD_SHARED=ON',
        '-DDEPOTKIT_BUILD_TESTS=ON', '-DCMAKE_DISABLE_FIND_PACKAGE_ZLIB=ON',
        '-DCMAKE_DISABLE_FIND_PACKAGE_zstd=ON', '-DZSTD_BUILD_SHARED=OFF',
        '-DZSTD_BUILD_STATIC=ON')
    run('cmake', '--build', BUILD, '--parallel', '2')
    run('ctest', '--test-dir', BUILD, '--output-on-failure')
    dependencies = subprocess.check_output(['ldd', str(BUILD / 'libdepotkit.so')], text=True)
    if 'not found' in dependencies or 'libQt' in dependencies:
        raise RuntimeError(f'Invalid engine runtime dependencies: {dependencies}')
    OUTPUT.mkdir(parents=True)
    resources = {}
    for source, name in (
        (BUILD / 'libdepotkit.so', 'libdepotkit.so'),
        (SOURCE / 'LICENSE', 'depotkit-LICENSE.txt'),
        (SOURCE / 'NOTICE', 'depotkit-NOTICE.txt'),
        (BUILD / '_deps/zlib-src/LICENSE', 'zlib-LICENSE.txt'),
        (BUILD / '_deps/zstd-src/LICENSE', 'zstd-LICENSE.txt'),
    ):
        shutil.copyfile(source, OUTPUT / name)
        resources[f'../target/depotkit-runtime/{name}'] = name
    (ROOT / 'src-tauri/tauri.linux.conf.json').write_text(json.dumps({
        'bundle': {'resources': resources}
    }, indent=2) + '\n')
    print(dependencies)


if __name__ == '__main__':
    main()
