# -*- coding: utf-8 -*-
"""R7 文档回写之后的门禁复跑：终端只打 ASCII 摘要，全文落盘。"""
import hashlib
import subprocess
import sys

out = subprocess.run([sys.executable, '-X', 'utf8', 'tools/check_architecture.py'],
                     capture_output=True, text=True, encoding='utf-8', errors='replace', cwd='.')
body = (out.stdout or '') + (out.stderr or '')
open('scratch_r7/gate_after_docs.txt', 'w', encoding='utf-8').write(body)
reds = [l for l in body.split('\n') if '✗' in l or 'FAIL' in l.upper() and 'PASSED' not in l.upper()]
print('rc', out.returncode)
print('bytes', len(body.encode('utf-8')))
print('redlines', len(reds))
for r in reds[:20]:
    print('  ', r.encode('ascii', 'backslashreplace').decode()[:160])
tail = [l for l in body.split('\n') if l.strip()][-3:]
for t in tail:
    print('tail:', t.encode('ascii', 'backslashreplace').decode()[:200])
print('sha', hashlib.sha256(body.encode('utf-8')).hexdigest()[:16])
