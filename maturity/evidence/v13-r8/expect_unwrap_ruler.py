"""V13 文中三处 `.expect(`/`.unwrap(` 计数的仓内复算件（V13 第 8 轮 B 轮）。

起草时那三份 `logs/s24_*.txt` 落在 .gitignore 的 `/logs/` 里、仓内不可达，
所以把 V13 引用的这几颗数字换成一把**写死在仓里、可复跑**的尺子：

  尺子 D：生产文件 = `crates/*/src/**/*.rs` 里路径不含 `test` 的那些；
          再把 `#[cfg(test)]` 标注的条目（`mod tests;` 声明、`mod tests { … }` 区块、
          紧随标注的 `fn`/`impl`/`struct`/`enum`/`trait`）整段扣掉。
  尺子 B：同一批生产文件，但从文件里第一颗 `#[cfg(test)]` / `mod tests` 起整段截断
          ——这把尺子复现 V13 第 103 行"剔掉之后只有 61 处"与"qx-strategy 33"。

打印三件事：两把尺子的全仓计数、qx-api/src/lib.rs 里 `.expect(` 的真实行号清单、
以及 V13 第 278 行原文那七颗行号各自落在哪一行（用来核对文档指针对不对得上代码）。
"""
import pathlib
import re

ROOT = pathlib.Path(__file__).resolve().parents[3]  # maturity/evidence/v13-r8/<本文件> → 仓库根
NEEDLES = (".unwrap(", ".expect(", "panic!", "todo!", "unimplemented!")


def prod_files() -> list[pathlib.Path]:
    out = []
    for c in sorted((ROOT / "crates").iterdir()):
        src = c / "src"
        if not src.is_dir():
            continue
        out += [
            f
            for f in sorted(src.rglob("*.rs"))
            if "test" not in " ".join(p.lower() for p in f.relative_to(ROOT).parts)
        ]
    return out


def ruler_d(lines: list[str]) -> list[tuple[int, str]]:
    keep: list[tuple[int, str]] = []
    i = 0
    while i < len(lines):
        line = lines[i]
        if line.strip().startswith("#[cfg(test)]"):
            j = i + 1
            while j < len(lines) and not lines[j].strip():
                j += 1
            head = lines[j].strip() if j < len(lines) else ""
            if re.match(r"^(pub(\([a-z]+\))? )?mod [a-z_]+;", head):
                i = j + 1
                continue
            if re.match(r"^(pub(\([a-z]+\))? )?(async )?fn [a-z_]+", head) or re.match(
                r"^(pub(\([a-z]+\))? )?(impl|struct|enum|trait)\b", head
            ):
                depth, started = 0, False
                while j < len(lines):
                    depth += lines[j].count("{") - lines[j].count("}")
                    started = started or "{" in lines[j]
                    j += 1
                    if started and depth <= 0:
                        break
                    if not started and lines[j - 1].rstrip().endswith(";"):
                        break
                i = j
                continue
        keep.append((i + 1, line))
        i += 1
    return keep


def ruler_b(lines: list[str]) -> list[tuple[int, str]]:
    cut = len(lines)
    for i, line in enumerate(lines):
        s = line.strip()
        if s.startswith("#[cfg(test)]") or re.match(r"^(pub )?mod tests\b", s):
            cut = i
            break
    return [(i + 1, l) for i, l in enumerate(lines[:cut])]


def measure(ruler, files):
    tot = dict.fromkeys(NEEDLES, 0)
    per: dict[str, dict[str, int]] = {}
    for f in files:
        ls = ruler(f.read_text(encoding="utf-8").splitlines())
        crate = f.relative_to(ROOT).parts[1]
        local = per.setdefault(crate, dict.fromkeys(NEEDLES, 0))
        for n in NEEDLES:
            c = sum(l.count(n) for _, l in ls)
            local[n] += c
            tot[n] += c
    return tot, per


FILES = prod_files()
print(f"# 尺子复算件（生产文件 {len(FILES)} 份）")
for name, ruler in (("D 扣掉 cfg(test) 条目", ruler_d), ("B 截到第一颗 cfg(test) 之前", ruler_b)):
    tot, per = measure(ruler, FILES)
    top = "、".join(f"{k} {v['.unwrap(']}" for k, v in sorted(per.items(), key=lambda kv: -kv[1][".unwrap("])[:4])
    print(f"\n== 尺子 {name}")
    print("   全仓：" + "  ".join(f"{k}={v}" for k, v in tot.items()))
    print(f"   unwrap 前列：{top}")
    print(f"   qx-api .expect( = {per.get('qx-api', dict.fromkeys(NEEDLES, 0))['.expect(']}")

LIB = (ROOT / "crates/qx-api/src/lib.rs").read_text(encoding="utf-8").splitlines()
pins = [n for n, l in ruler_d(LIB) if ".expect(" in l]
print("\n== §3 qx-api/src/lib.rs 里 `.expect(` 的真实行号（尺子 D）")
print(f"   共 {len(pins)} 颗，前 8 颗：{pins[:8]}")
print("\n== V13 第 278 行原文那七颗行号各自落在哪一行")
for n in (77, 97, 102, 113, 126, 154, 161):
    text = LIB[n - 1].strip()
    print(f"   :{n} → {'空行' if not text else text[:72]}")
