#!/usr/bin/env python3
"""兼容原命令行；uv 管理新公共库依赖。"""

import os
import sys
from pathlib import Path

if __name__ == '__main__':
    directory = Path(__file__).resolve().parent / 'mcp-e2e'
    os.execvp(
        'uv',
        [
            'uv',
            'run',
            '--project',
            str(directory),
            'python',
            str(directory / 'views.py'),
            *sys.argv[1:],
        ],
    )
