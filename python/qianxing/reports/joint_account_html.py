"""Offline interactive view over Rust-verified joint-account report facts."""

from __future__ import annotations

import html
import json
import os
import tempfile
from collections.abc import Mapping
from dataclasses import asdict, is_dataclass
from pathlib import Path
from typing import Any

from qianxing.app import read_multi_account_joint_report_v1


MAX_ACCOUNTS = 32
MAX_EQUITY_ROWS = 2_000
MAX_FILL_ROWS = 500
MAX_EVENT_ROWS = 300
MAX_INLINE_REPORT_BYTES = 32 * 1024 * 1024


def _escape(value: Any) -> str:
    if value is None:
        text = "—"
    elif isinstance(value, (dict, list, tuple)):
        text = json.dumps(value, ensure_ascii=False, sort_keys=True, default=str)
    else:
        text = str(value)
    return html.escape(text, quote=True)


def _inline_json(value: Any) -> str:
    return (
        json.dumps(value, ensure_ascii=False, separators=(",", ":"), default=str)
        .replace("&", "\\u0026")
        .replace("<", "\\u003c")
        .replace(">", "\\u003e")
        .replace("\u2028", "\\u2028")
        .replace("\u2029", "\\u2029")
    )


def _validate_report(report: Mapping[str, Any]) -> tuple[Mapping[str, Any], list[Mapping[str, Any]]]:
    if report.get("schema_version") != 1:
        raise ValueError("unsupported MultiAccountJointResearchReport schema_version")
    verification = report.get("verification")
    if not isinstance(verification, Mapping) or verification.get("verified") is not True:
        raise ValueError("joint-account HTML requires a Rust-verified report")
    if verification.get("mismatches"):
        raise ValueError("verified joint-account report contains mismatches")
    summary = report.get("summary")
    if not isinstance(summary, Mapping):
        raise ValueError("joint-account report summary must be an object")
    accounts = report.get("accounts")
    equity = report.get("equity")
    if not isinstance(accounts, list) or not 1 <= len(accounts) <= MAX_ACCOUNTS:
        raise ValueError(f"joint-account report must contain 1..={MAX_ACCOUNTS} accounts")
    if not isinstance(equity, Mapping) or not isinstance(equity.get("rows"), list):
        raise ValueError("joint-account report equity rows are invalid")
    if len(equity["rows"]) > MAX_EQUITY_ROWS:
        raise ValueError("joint-account report exceeds the verified equity sample bound")
    fills = report.get("fills")
    if (
        not isinstance(fills, Mapping)
        or not isinstance(fills.get("rows"), list)
        or len(fills["rows"]) > MAX_FILL_ROWS
    ):
        raise ValueError("joint-account report fill rows are invalid or exceed their bound")
    for index, account in enumerate(accounts):
        if not isinstance(account, Mapping):
            raise ValueError(f"joint-account report account[{index}] must be an object")
        for key, limit in (("fill_rows", MAX_FILL_ROWS), ("event_rows", MAX_EVENT_ROWS)):
            rows = account.get(key)
            if not isinstance(rows, list) or len(rows) > limit:
                raise ValueError(f"joint-account report account[{index}].{key} exceeds its bound")
    return summary, accounts


def _render_verified_report(report: Mapping[str, Any]) -> str:
    summary, accounts = _validate_report(report)
    verification = report["verification"]
    equity = report["equity"]
    fills = report.get("fills", {})
    run_id = report.get("run_id")
    cards = (
        ("联合运行", run_id),
        ("结算币种", report.get("settlement_currency")),
        ("账户数", summary.get("accounts_total")),
        ("标的数", summary.get("markets_total")),
        ("初始资金 raw", summary.get("initial_cash_raw")),
        ("期末权益 raw", summary.get("final_equity_raw")),
        ("收益 bps", summary.get("return_bps")),
        ("最大回撤 bps", summary.get("max_drawdown_bps")),
        ("成交数", summary.get("successful_fills")),
    )
    card_html = "".join(
        f'<div class="card"><span>{_escape(label)}</span><strong>{_escape(value)}</strong></div>'
        for label, value in cards
    )
    account_rows = "".join(
        "<tr>"
        + "".join(
            f"<td>{_escape(account.get(key))}</td>"
            for key in (
                "exchange_id",
                "account_id",
                "run_id",
                "initial_cash_raw",
                "final_equity_raw",
                "return_bps",
                "max_drawdown_bps",
                "markets_total",
                "strategies_total",
                "successful_fills",
            )
        )
        + "</tr>"
        for account in accounts
    )
    checks = verification.get("checks", [])
    checks_html = "".join(f"<li>{_escape(item)}</li>" for item in checks if isinstance(item, str))
    warning = ""
    if summary.get("complete") is not True:
        warning = '<p class="warning">联合报告标记为不完整；请勿把汇总排名解释为完整账户表现。</p>'
    if equity.get("truncated"):
        warning += '<p class="warning">联合权益曲线经过有界抽样，图表只展示报告返回的采样点。</p>'
    if fills.get("truncated"):
        warning += '<p class="warning">联合成交样本经过截断；账户成交表仅显示各账户的有界样本。</p>'

    payload = _inline_json(report)
    if len(payload.encode("utf-8")) > MAX_INLINE_REPORT_BYTES:
        raise ValueError(
            f"joint-account report exceeds the {MAX_INLINE_REPORT_BYTES}-byte HTML payload bound"
        )
    title = f"牵星联合账户研究报告 — {_escape(run_id)}"
    return f"""<!doctype html>
<html lang="zh-CN"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>{title}</title><style>
:root{{--ink:#17212b;--muted:#61717e;--line:#dbe3e8;--panel:#fff;--bg:#f3f6f8;--accent:#176b87;--warn:#fff4d6}}
*{{box-sizing:border-box}}body{{margin:0;background:var(--bg);color:var(--ink);font:14px/1.5 system-ui,"Microsoft YaHei",sans-serif}}
main{{max-width:1500px;margin:0 auto;padding:26px}}h1{{font-size:24px;margin:0 0 18px}}h2{{font-size:18px;margin:0 0 12px}}h3{{font-size:15px;margin:18px 0 8px}}
section{{background:var(--panel);border:1px solid var(--line);border-radius:10px;padding:18px;margin:14px 0;box-shadow:0 1px 3px #14202a0a}}
.cards{{display:grid;grid-template-columns:repeat(auto-fit,minmax(145px,1fr));gap:9px}}.card{{padding:11px;background:#f7fafb;border:1px solid var(--line);border-radius:8px;min-width:0}}.card span{{display:block;color:var(--muted);font-size:12px}}.card strong{{display:block;overflow-wrap:anywhere;margin-top:4px}}
.hint{{color:var(--muted);margin:8px 0}}.warning{{background:var(--warn);border-left:4px solid #d69e00;padding:9px 12px;margin:10px 0}}canvas{{width:100%;height:auto;border:1px solid var(--line);border-radius:7px;background:#fff;margin:8px 0}}
.toolbar{{display:flex;gap:8px;align-items:center;margin:10px 0}}input,select,button{{font:inherit;padding:8px 10px;border:1px solid #bdcbd3;border-radius:6px;background:#fff}}input{{min-width:230px;flex:1}}button{{cursor:pointer;color:#fff;background:var(--accent);border-color:var(--accent)}}.table-wrap{{overflow:auto}}table{{width:100%;border-collapse:collapse;min-width:900px}}th,td{{text-align:left;padding:8px;border-bottom:1px solid var(--line);vertical-align:top;white-space:pre-wrap;overflow-wrap:anywhere}}th{{background:#f5f8f9;position:sticky;top:0}}tr:hover td{{background:#f8fbfc}}footer{{font-size:12px;color:var(--muted);padding:8px}}
@media(max-width:700px){{main{{padding:12px}}section{{padding:12px}}.toolbar{{align-items:stretch;flex-direction:column}}input{{min-width:0}}}}
</style></head><body><main><h1>{title}</h1>
<section><h2>组合概览</h2><div class="cards">{card_html}</div>{warning}<p class="hint">报告由 Rust 读模型复核后生成。账户资金、仓位和风险保持隔离；图表不会跨账户净额，也不会跨币种折算。</p></section>
<section><h2>核验结果</h2><p>verified: <strong>true</strong> · Run ID: {_escape(verification.get('run_id'))} · result hash: {_escape(verification.get('result_hash'))} · data fingerprint: {_escape(verification.get('data_fingerprint'))}</p><ul>{checks_html}</ul></section>
<section><h2>联合权益曲线</h2><canvas id="equityChart" width="1200" height="300" aria-label="经过 Rust 核验的联合权益曲线"></canvas><p id="equityHover" class="hint">将鼠标移到曲线上查看时间戳和精确 equity_raw。</p></section>
<section><h2>账户对比</h2><canvas id="accountChart" width="1200" height="280" aria-label="各独立账户收益和最大回撤比较"></canvas><div class="toolbar"><input id="accountFilter" type="search" placeholder="筛选交易所、账户或运行 ID"><button id="exportAccounts" type="button">导出账户 CSV</button></div><div class="table-wrap"><table id="accountTable"><thead><tr><th>交易所</th><th>账户</th><th>Run ID</th><th>初始资金 raw</th><th>期末权益 raw</th><th>收益 bps</th><th>最大回撤 bps</th><th>标的数</th><th>策略数</th><th>成交数</th></tr></thead><tbody>{account_rows}</tbody></table></div><p class="hint">收益和回撤直接取自已核验的账户报告；raw 金额作为字符串显示，不经过浮点转换。</p></section>
<section><h2>账户成交与事件</h2><div class="toolbar"><label for="accountSelect">账户</label><select id="accountSelect"></select><button id="exportFills" type="button">导出当前账户成交 CSV</button></div><p id="pageInfo" class="hint"></p><h3>成交样本</h3><div class="table-wrap"><table id="fillTable"><thead></thead><tbody></tbody></table></div><h3>EventLog 样本</h3><div class="table-wrap"><table id="eventTable"><thead></thead><tbody></tbody></table></div><p class="hint">各交易所账户拥有独立事件序列。报告只展示读取时请求的账户分页，不构造跨交易所全局事件顺序。</p></section>
<section><h2>报告范围</h2><p class="hint">收益、最大回撤、成交和身份来自 Rust 共享读模型。页面不重新计算交易指标；它只绘制已核验的联合权益采样，并展示每个账户独立的成交和事件样本。被截断的采样不会被描述为完整明细。</p></section>
<footer>牵星离线多账户研究报告 · 内嵌数据，无外部网络资源</footer></main>
<script type="application/json" id="jointReportData">{payload}</script><script>
const report=JSON.parse(document.getElementById('jointReportData').textContent),accounts=report.accounts||[];
const equityRows=(report.equity||{{}}).rows||[],chart=document.getElementById('equityChart'),hover=document.getElementById('equityHover');
const drawEquity=()=>{{const dpr=window.devicePixelRatio||1,w=chart.clientWidth||900,h=280;chart.width=w*dpr;chart.height=h*dpr;const c=chart.getContext('2d');c.setTransform(dpr,0,0,dpr,0,0);c.clearRect(0,0,w,h);if(!equityRows.length){{c.fillStyle='#61717e';c.fillText('报告没有权益采样点',16,24);return;}}const points=equityRows.map(p=>({{ts:String(p.ts),raw:String(p.equity_raw),v:BigInt(p.equity_raw)}}));let min=points[0].v,max=min;points.forEach(p=>{{if(p.v<min)min=p.v;if(p.v>max)max=p.v;}});let range=max-min;if(range===0n)range=1n;const left=130,right=w-18,top=24,bottom=h-28;c.strokeStyle='#dbe3e8';c.beginPath();c.moveTo(left,top);c.lineTo(left,bottom);c.lineTo(right,bottom);c.stroke();c.fillStyle='#61717e';c.font='11px system-ui';c.fillText(`raw ${{min}}`,8,h-8);c.fillText(`raw ${{max}}`,8,16);c.strokeStyle='#176b87';c.lineWidth=2;c.beginPath();points.forEach((p,i)=>{{const x=left+(right-left)*i/Math.max(1,points.length-1),y=bottom-Number((p.v-min)*1000000n/range)/1000000*(bottom-top);if(i===0)c.moveTo(x,y);else c.lineTo(x,y);}});c.stroke();chart._points=points;chart._bounds={{left,right}};}};
chart.addEventListener('mousemove',e=>{{if(!chart._points)return;const rect=chart.getBoundingClientRect(),x=e.clientX-rect.left,{{left,right}}=chart._bounds;if(x<left||x>right)return;const i=Math.max(0,Math.min(chart._points.length-1,Math.round((x-left)/(right-left)*Math.max(1,chart._points.length-1)))),p=chart._points[i];hover.textContent=`ts=${{p.ts}} · equity_raw=${{p.raw}}`;}});window.addEventListener('resize',drawEquity);drawEquity();
const accountCanvas=document.getElementById('accountChart');
const drawAccounts=()=>{{const dpr=window.devicePixelRatio||1,w=accountCanvas.clientWidth||900,h=260;accountCanvas.width=w*dpr;accountCanvas.height=h*dpr;const c=accountCanvas.getContext('2d');c.setTransform(dpr,0,0,dpr,0,0);c.clearRect(0,0,w,h);const top=30,bottom=h-35,left=35,right=w-18,slot=(right-left)/Math.max(1,accounts.length),scale=Math.max(1,...accounts.flatMap(a=>[Math.abs(Number(a.return_bps)||0),Math.abs(Number(a.max_drawdown_bps)||0)]));c.font='11px system-ui';accounts.forEach((a,i)=>{{const center=left+slot*(i+.5),ret=Number(a.return_bps)||0,dd=Number(a.max_drawdown_bps)||0,bw=Math.min(18,slot*.28),base=(top+bottom)/2,span=(bottom-top)/2;const rh=Math.abs(ret)/scale*span,dh=Math.abs(dd)/scale*span;c.fillStyle=ret>=0?'#18815c':'#c65353';c.fillRect(center-bw-2,ret>=0?base-rh:base,bw,Math.max(1,rh));c.fillStyle='#c17c16';c.fillRect(center+2,base,bw,Math.max(1,dh));if(slot>32){{c.save();c.translate(center,bottom+2);c.rotate(-.6);c.fillStyle='#61717e';c.fillText(`${{a.exchange_id}}:${{a.account_id}}`,0,0);c.restore();}}}});c.fillStyle='#18815c';c.fillRect(left,8,10,10);c.fillStyle='#17212b';c.fillText('收益 bps',left+15,17);c.fillStyle='#c17c16';c.fillRect(left+100,8,10,10);c.fillStyle='#17212b';c.fillText('最大回撤 bps',left+115,17);}};
window.addEventListener('resize',drawAccounts);drawAccounts();
const accountSelect=document.getElementById('accountSelect'),fillHead=document.querySelector('#fillTable thead'),fillBody=document.querySelector('#fillTable tbody'),eventHead=document.querySelector('#eventTable thead'),eventBody=document.querySelector('#eventTable tbody'),pageInfo=document.getElementById('pageInfo');
accounts.forEach((a,i)=>{{const o=document.createElement('option');o.value=String(i);o.textContent=`${{a.exchange_id}} · ${{a.account_id}}`;accountSelect.appendChild(o);}});
const renderRows=(head,body,rows)=>{{head.replaceChildren();body.replaceChildren();if(!rows.length){{const tr=document.createElement('tr'),td=document.createElement('td');td.textContent='当前报告没有该类样本';td.colSpan=1;tr.appendChild(td);body.appendChild(tr);return;}}const keys=[...new Set(rows.flatMap(row=>Object.keys(row)))].slice(0,16),hr=document.createElement('tr');keys.forEach(key=>{{const th=document.createElement('th');th.textContent=key;hr.appendChild(th);}});head.appendChild(hr);rows.forEach(row=>{{const tr=document.createElement('tr');keys.forEach(key=>{{const td=document.createElement('td'),value=row[key];td.textContent=typeof value==='object'&&value!==null?JSON.stringify(value):String(value??'');tr.appendChild(td);}});body.appendChild(tr);}});}};
const selected=()=>accounts[Number(accountSelect.value)]||null;const showAccount=()=>{{const a=selected();if(!a)return;renderRows(fillHead,fillBody,a.fill_rows||[]);renderRows(eventHead,eventBody,a.event_rows||[]);const page=`EventLog ${{a.event_rows_offset??0}} / ${{a.event_rows_total??0}} · 本页 ${{(a.event_rows||[]).length}} / ${{a.event_rows_limit??0}}`;const next=a.event_rows_next_offset===null||a.event_rows_next_offset===undefined?'':' · 下一页 offset='+a.event_rows_next_offset;const trunc=a.event_rows_truncated?' · 当前分页仍有更多事件':'';pageInfo.textContent=page+next+trunc+` · 成交样本 ${{(a.fill_rows||[]).length}} / ${{a.fill_rows_total??0}}`+(a.fill_rows_truncated?'（样本已截断）':'');}};
accountSelect.addEventListener('change',showAccount);if(accounts.length){{accountSelect.value='0';showAccount();}}
const filter=document.getElementById('accountFilter'),accountTable=document.getElementById('accountTable');filter.addEventListener('input',()=>{{const q=filter.value.toLowerCase();[...accountTable.tBodies[0].rows].forEach(row=>{{row.hidden=!row.textContent.toLowerCase().includes(q);}});}});
const csv=(rows,keys)=>{{const quote=value=>{{let s=String(value??'');if(/^[=+@\\-\\t\\r]/.test(s))s="'"+s;return '"'+s.replaceAll('"','""')+'"';}};return '\\ufeff'+[keys.map(quote).join(','),...rows.map(row=>keys.map(k=>quote(typeof row[k]==='object'&&row[k]!==null?JSON.stringify(row[k]):row[k])).join(','))].join('\\r\\n');}};
document.getElementById('exportAccounts').addEventListener('click',()=>{{const keys=['exchange_id','account_id','run_id','initial_cash_raw','final_equity_raw','return_bps','max_drawdown_bps','markets_total','strategies_total','successful_fills'],blob=new Blob([csv(accounts,keys)],{{type:'text/csv;charset=utf-8'}}),a=document.createElement('a');a.href=URL.createObjectURL(blob);a.download='qianxing-joint-accounts.csv';a.click();URL.revokeObjectURL(a.href);}});
document.getElementById('exportFills').addEventListener('click',()=>{{const a=selected();if(!a)return;const rows=a.fill_rows||[],keys=[...new Set(rows.flatMap(row=>Object.keys(row)))],blob=new Blob([csv(rows,keys)],{{type:'text/csv;charset=utf-8'}}),link=document.createElement('a');link.href=URL.createObjectURL(blob);link.download=`${{a.exchange_id}}-${{a.account_id}}-fills.csv`;link.click();URL.revokeObjectURL(link.href);}});
</script></body></html>"""


def render_multi_account_joint_report_html(
    outcome: Any,
    *,
    joint_account_event_pages: Mapping[str, Mapping[str, Mapping[str, int]]] | None = None,
) -> str:
    """Verify a joint outcome through qx-app, then render its bounded offline report."""
    report = read_multi_account_joint_report_v1(
        outcome,
        joint_account_event_pages=(
            None if joint_account_event_pages is None else dict(joint_account_event_pages)
        ),
    )
    if not isinstance(report, Mapping):
        raise ValueError("Rust joint-account report reader returned a non-object")
    return _render_verified_report(report)


def _as_mapping(value: Any) -> Mapping[str, Any]:
    if is_dataclass(value) and not isinstance(value, type):
        return asdict(value)
    if isinstance(value, Mapping):
        return value
    raise TypeError("joint-account outcome must be a dataclass or mapping")


def _protected_paths(value: Any) -> list[str]:
    protected: set[str] = set()

    def visit(item: Any) -> None:
        if is_dataclass(item) and not isinstance(item, type):
            visit(asdict(item))
        elif isinstance(item, Mapping):
            for key, child in item.items():
                name = str(key)
                if isinstance(child, str) and (
                    name.endswith("_path")
                    or name in {"run_record", "manifest_path"}
                ):
                    protected.add(child)
                elif name == "artifact_locations" and isinstance(child, Mapping):
                    protected.update(str(path) for path in child.values() if isinstance(path, str))
                visit(child)
        elif isinstance(item, (list, tuple)):
            for child in item:
                visit(child)

    visit(value)
    return sorted(protected)


def _default_report_path(outcome: Mapping[str, Any]) -> Path:
    artifact_path = outcome.get("artifact_path")
    if not isinstance(artifact_path, str) or not artifact_path:
        raise ValueError("joint outcome has no artifact_path; pass an explicit HTML output path")
    source = Path(artifact_path)
    return source.with_suffix(".report.html") if source.suffix else Path(f"{source}.report.html")


def write_multi_account_joint_report_html(
    outcome: Any,
    output_path: str | os.PathLike[str] | None = None,
    *,
    joint_account_event_pages: Mapping[str, Mapping[str, Mapping[str, int]]] | None = None,
) -> Path:
    """Atomically write a verified report without overwriting any referenced artifact."""
    outcome_mapping = _as_mapping(outcome)
    target = Path(output_path) if output_path is not None else _default_report_path(outcome_mapping)
    resolved = target.resolve(strict=False)
    target_key = os.path.normcase(str(resolved))
    for source in _protected_paths(outcome):
        if os.path.normcase(str(Path(source).resolve(strict=False))) == target_key:
            raise ValueError(f"HTML report output would overwrite a joint-account artifact: {source}")

    content = render_multi_account_joint_report_html(
        outcome,
        joint_account_event_pages=joint_account_event_pages,
    )
    target.parent.mkdir(parents=True, exist_ok=True)
    temporary: Path | None = None
    try:
        with tempfile.NamedTemporaryFile(
            mode="w",
            encoding="utf-8",
            newline="\n",
            dir=target.parent,
            prefix=f".{target.name}.",
            suffix=".tmp",
            delete=False,
        ) as handle:
            temporary = Path(handle.name)
            handle.write(content)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temporary, target)
    finally:
        if temporary is not None and temporary.exists():
            temporary.unlink()
    return target


__all__ = [
    "render_multi_account_joint_report_html",
    "write_multi_account_joint_report_html",
]
