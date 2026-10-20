#!/usr/bin/env python3
"""从用户品牌源图更新平台图标。macOS 运行；Windows 使用预生成资源。"""
from pathlib import Path
import subprocess
import sys


def main():
    root = Path(__file__).resolve().parents[1]
    if sys.platform != 'darwin':
        raise SystemExit('请在 macOS 上更新品牌资源；Windows 构建无需运行此脚本。')
    cache = root / 'target/icon-tools/swift-cache'
    cache.mkdir(parents=True, exist_ok=True)
    subprocess.run([
        'swift', '-module-cache-path', str(cache),
        str(root / 'scripts/gen-icons.swift'), str(root),
    ], check=True)


if __name__ == '__main__':
    main()
