# -*- coding: utf-8 -*-
"""把 §54 点名的三份 R6 取证脚本改成自足可跑：ROOT 按新落点反推，依赖文件复制进证据目录。

落点：maturity/evidence/v11-r6/（R7 轮迁入）。迁入之后 ROOT 反推仍按旧的 scratch_r6/ 位置，
所以"可复跑"那句话在迁入的当下是假的——这一步把它补真。逐颗锚点断言命中数，二进制读写。
"""
import hashlib
from pathlib import Path

EV = Path("maturity/evidence/v11-r6")


def patch(rel, pairs, must_not=()):
    p = EV / rel
    raw = p.read_bytes()
    before = hashlib.sha256(raw).hexdigest()[:16]
    text = raw.decode("utf-8")
    for old, new, count in pairs:
        got = text.count(old)
        assert got == count, "%s 锚点命中 %d，期望 %d：%r" % (rel, got, count, old[:60])
        text = text.replace(old, new)
    for needle in must_not:
        assert needle not in text, "%s 仍含 %r" % (rel, needle)
    data = text.encode("utf-8")
    assert data.count(b"\r\n") == 0
    p.write_bytes(data)
    print("patched %-24s sha %s -> %s lines %d" % (
        rel, before, hashlib.sha256(data).hexdigest()[:16], data.count(b"\n")))


# A. 依赖：bare_orphan.py 从 scratch 复制进证据目录，ROOT 反推按新位置
src = Path("scratch_r6/bare_orphan.py").read_bytes()
assert b'ROOT = Path(os.path.dirname(os.path.abspath(__file__))).parent\n' in src
dst = (EV / "bare_orphan.py")
already = dst.exists()
if not already:
    dst.write_bytes(src.replace(
        b'ROOT = Path(os.path.dirname(os.path.abspath(__file__))).parent',
        b'ROOT = Path(os.path.dirname(os.path.abspath(__file__))).parents[2]'))
print("copied bare_orphan.py %s" % ("skip" if already else "ok"))

# B. gate_shape_audit.py：ROOT 由 dirname(dirname(__file__)) 改成按新落点反推
patch("gate_shape_audit.py", [(
    'ROOT = Path(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))',
    'ROOT = Path(os.path.abspath(__file__)).resolve().parents[3]', 1)],
    must_not=("scratch_r6",))

# C. probe_r69h.py：ROOT、依赖脚本、快照清单三处一起改到证据目录
patch("probe_r69h.py", [
    ('ROOT = Path(__file__).resolve().parent.parent',
     'HERE = Path(__file__).resolve().parent\nROOT = HERE.parents[2]', 1),
    ('src = (ROOT / "scratch_r6" / "bare_orphan.py").read_text(encoding="utf-8")',
     'src = (HERE / "bare_orphan.py").read_text(encoding="utf-8")', 1),
    ('repr(str(ROOT / "scratch_r6" / "x.py"))', 'repr(str(HERE / "bare_orphan.py"))', 1),
    ('sorted(ROOT.glob("scratch_r6/V11.md.snap*"))', 'sorted(HERE.glob("V11.md.snap*"))', 1),
])

# D. mutate_r68.py：两条外部路径同批改口（取证记录本身不动，只改脚本的取数路径）
patch("mutate_r68.py", [('scratch_r6/', 'maturity/evidence/v11-r6/', 3)])
