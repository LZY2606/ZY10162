'use strict';
let state = { segments: [], markers: [], exchanges: [], runs: [] };
let selectedRun = null;
let lastResult = null;

const $ = (id) => document.getElementById(id);
const esc = (s) => String(s ?? '').replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]));

function banner(msg, kind = '') {
  const b = $('banner');
  b.textContent = msg;
  b.className = 'banner' + (kind ? ' ' + kind : '');
  b.classList.remove('hidden');
  if (!kind) setTimeout(() => b.classList.add('hidden'), 4000);
}

async function api(path, opts) {
  const res = await fetch(path, opts);
  const ct = res.headers.get('content-type') || '';
  const body = ct.includes('json') ? await res.json() : await res.text();
  if (!res.ok) throw new Error(body.error || body || ('HTTP ' + res.status));
  return body;
}

async function refresh() {
  state = await api('/api/state');
  render();
}

function kindLabel(k) {
  return { layer: '季节层', volcano: '火山灰', isotope: '同位素', anchor: '锚点' }[k] || k;
}

function renderSegments() {
  const tb = document.querySelector('#segments-table tbody');
  tb.innerHTML = state.segments.map((s) => `
    <tr class="${s.present ? '' : 'gap-row'}">
      <td>${esc(s.id)}</td>
      <td>${s.top}</td><td>${s.bottom}</td>
      <td>${s.present ? '完整' : '<b>缺芯</b>'}</td>
      <td>${s.present ? '—' : s.gap_min_years}</td>
      <td>${esc(s.note)}</td>
      <td>${s.present ? '' : '<button data-seg="' + esc(s.id) + '">改边界</button>'}
          <button data-seg-gap="${s.present ? '' : esc(s.id)}" data-seg-edit="${esc(s.id)}" style="display:none"></button>
          <button data-seg-edit="${esc(s.id)}">编辑</button></td>
    </tr>`).join('');
}

function renderMarkers() {
  const tb = document.querySelector('#markers-table tbody');
  tb.innerHTML = state.markers.map((m) => `
    <tr style="${m.excluded ? 'opacity:.45' : ''}">
      <td>${esc(m.id)}</td><td>${m.depth}</td>
      <td>${kindLabel(m.kind)}</td>
      <td>${m.hard ? '<span class="chip">硬</span>' : '<span class="chip soft">软</span>'}</td>
      <td>[${m.lo}, ${m.hi}]</td>
      <td>${m.mean ?? '—'}</td><td>${m.sd ?? '—'}</td>
      <td>${(m.alt_means || []).join('/') || '—'}</td>
      <td>${esc(m.exchange_group || '—')}</td>
      <td>${m.weight}</td>
      <td><input type="checkbox" ${m.excluded ? 'checked' : ''} data-exclude="${esc(m.id)}" /></td>
      <td>${esc(m.note)}</td>
      <td>
        <button data-marker="${esc(m.id)}">编辑</button>
      </td>
    </tr>`).join('');
}

function renderExchanges() {
  const box = $('exchanges');
  if (!state.exchanges.length) { box.innerHTML = '<span class="small">（无交换组）</span>'; return; }
  box.innerHTML = state.exchanges.map((e) => `
    <div>交换组 <b>${esc(e.group)}</b>：
      <label><input type="radio" name="ex-${esc(e.group)}" ${!e.swapped ? 'checked' : ''} data-ex="${esc(e.group)}" data-val="0" /> 按深度正序</label>
      <label><input type="radio" name="ex-${esc(e.group)}" ${e.swapped ? 'checked' : ''} data-ex="${esc(e.group)}" data-val="1" /> 对调归属</label>
    </div>`).join('');
}

function renderRuns() {
  const tb = document.querySelector('#runs-table tbody');
  tb.innerHTML = state.runs.map((r) => `
    <tr class="${r.status}">
      <td>#${r.seq}</td><td>${r.kind}</td>
      <td class="status">${r.status === 'feasible' ? '可行' : '已拒绝'}</td>
      <td title="${esc(r.reason)}">${esc(r.reason)}</td>
      <td>${r.parent_seq ? '#' + r.parent_seq : ''}</td>
      <td><button data-run="${r.seq}">查看</button></td>
    </tr>`).join('');
  const opts = state.runs.filter((r) => r.status === 'feasible')
    .map((r) => `<option value="${r.seq}" ${String(selectedRun) === String(r.seq) ? 'selected' : ''}>#${r.seq} ${r.kind}</option>`).join('');
  $('base-run').innerHTML = opts;
  $('diff-a').innerHTML = opts;
  $('diff-b').innerHTML = opts;
  if (state.runs.length >= 2) {
    const feas = state.runs.filter((r) => r.status === 'feasible');
    if (feas.length >= 2) $('diff-b').value = feas[feas.length - 1].seq;
  }
}

function render() {
  renderSegments();
  renderMarkers();
  renderExchanges();
  renderRuns();
}

function fmt(v) { return (Math.round(v * 1000) / 1000).toString(); }

async function showRun(seq) {
  selectedRun = seq;
  const rec = await api('/api/runs/' + seq);
  const result = JSON.parse(rec.result_json);
  lastResult = result;
  let html = `<h2>运行 #${rec.seq} <span class="small">${rec.kind} · 种子 ${rec.seed} · ${rec.draws} 次抽样</span></h2>`;
  if (!result.feasible) {
    const c = result.conflict;
    html += `<div class="banner error"><b>拒绝发布：</b>${esc(c.reason)}<br/>
      最小冲突集${c.minimal ? '（已验证极小）' : ''}：
      ${c.markers.map((m) => `<span class="chip">${esc(m.id)}@${m.depth}m [${m.lo},${m.hi}]</span>`).join('')}
      ${c.gaps.length ? '缺芯 ' + c.gaps.map((g) => `<span class="chip gap">${esc(g)}</span>`).join('') : ''}
    </div>`;
  } else {
    html += `<table class="data"><thead><tr><th>深度</th><th>q05</th><th>中位数</th><th>q95</th><th>依赖锦标</th><th>依赖缺芯</th></tr></thead><tbody>` +
      result.nodes.map((n) => `<tr><td>${n.depth}</td><td>${fmt(n.q05)}</td><td><b>${fmt(n.median)}</b></td><td>${fmt(n.q95)}</td>
        <td class="small">${n.dep_markers.map((m) => `<span class="chip">${esc(m)}</span>`).join('')}</td>
        <td class="small">${n.dep_gaps.map((g) => `<span class="chip gap">${esc(g)}</span>`).join('')}</td></tr>`).join('') +
      '</tbody></table>';
    html += `<h2>局部累积率</h2><table class="data"><thead><tr><th>深度段</th><th>跨度</th><th>速率中位数${''}</th><th>q05</th><th>q95</th><th>类型</th></tr></thead><tbody>` +
      result.edges.map((e) => `<tr class="${e.is_gap ? 'gap-row' : ''}"><td>${e.depth_from}→${e.depth_to}</td>
        <td>${fmt(e.depth_to - e.depth_from)}m</td><td>${fmt(e.rate_median)}${e.is_gap ? ' 年(缺芯)' : ' 年/米'}</td>
        <td>${fmt(e.rate_q05)}</td><td>${fmt(e.rate_q95)}</td>
        <td>${e.is_gap ? '缺芯年数 ≥ ' + esc(e.gap_segment || '') : '年/米'}</td></tr>`).join('') +
      '</tbody></table>';
    if (result.warnings && result.warnings.length) {
      html += '<div class="banner warn">' + result.warnings.map(esc).join('<br/>') + '</div>';
    }
    if (rec.rerun_json) {
      const rr = JSON.parse(rec.rerun_json);
      html += `<div class="banner">增量重放：复用 <b>${rr.reused_node_count}</b> 个节点分位数，失效 <b>${rr.invalidated_node_count}</b> 个。
        ${rr.changes.detail.length ? '变更：' + rr.changes.detail.map(esc).join('；') : ''}</div>`;
    }
  }
  $('run-detail').innerHTML = html;
  drawChart(result);
}

function drawChart(result) {
  const cv = $('chart');
  const ctx = cv.getContext('2d');
  const W = cv.width, H = cv.height, M = 46;
  ctx.clearRect(0, 0, W, H);
  if (!result || !result.feasible) {
    ctx.fillStyle = '#8b98a5';
    ctx.font = '14px sans-serif';
    ctx.fillText('该运行无可行年尺（硬约束冲突）', M, H / 2);
    return;
  }
  const nodes = result.nodes;
  const dMax = Math.max(...nodes.map((n) => n.depth));
  const aMax = Math.max(...nodes.map((n) => n.q95));
  const X = (d) => M + (d / dMax) * (W - 2 * M);
  const Y = (a) => H - M - (a / aMax) * (H - 2 * M);

  ctx.strokeStyle = '#243040';
  ctx.lineWidth = 1;
  ctx.fillStyle = '#8b98a5';
  ctx.font = '11px sans-serif';
  for (let i = 0; i <= 5; i++) {
    const a = (aMax * i) / 5;
    ctx.beginPath(); ctx.moveTo(M, Y(a)); ctx.lineTo(W - M, Y(a)); ctx.stroke();
    ctx.fillText(Math.round(a) + '年', 6, Y(a) + 4);
  }
  for (let i = 0; i <= 6; i++) {
    const d = (dMax * i) / 6;
    ctx.fillText(d.toFixed(1) + 'm', X(d) - 12, H - 22);
  }

  // q05-q95 带
  ctx.fillStyle = 'rgba(61,126,166,.25)';
  ctx.beginPath();
  nodes.forEach((n, i) => { const x = X(n.depth), y = Y(n.q05); i ? ctx.lineTo(x, y) : ctx.moveTo(x, y); });
  for (let i = nodes.length - 1; i >= 0; i--) ctx.lineTo(X(nodes[i].depth), Y(nodes[i].q95));
  ctx.closePath(); ctx.fill();

  ctx.strokeStyle = '#7fc7ff';
  ctx.lineWidth = 2;
  ctx.beginPath();
  nodes.forEach((n, i) => { const x = X(n.depth), y = Y(n.median); i ? ctx.lineTo(x, y) : ctx.moveTo(x, y); });
  ctx.stroke();

  // 缺芯段
  (window.__segments || state.segments).filter((s) => !s.present).forEach((s) => {
    ctx.fillStyle = 'rgba(122,92,156,.28)';
    ctx.fillRect(X(s.top), M, X(s.bottom) - X(s.top), H - 2 * M);
    ctx.fillStyle = '#c9a8e8';
    ctx.fillText('缺芯 ' + s.id, X(s.top) + 4, M + 14);
  });

  ctx.fillStyle = '#9fd4ff';
  ctx.font = '13px sans-serif';
  ctx.fillText('年龄（年）', M, 18);
}

async function editMarker(id) {
  const m = state.markers.find((x) => x.id === id);
  const field = (label, def) => {
    const v = prompt(label, def);
    return v;
  };
  const mean = prompt('软分布均值（留空保持 ' + (m.mean ?? 'null') + '）', m.mean ?? '');
  const sd = prompt('软分布标准差（-1 表示无 sd）', m.sd ?? '-1');
  const excludedStr = confirm('是否暂时排除该锦标？确定=排除，取消=参与') ? '1' : '0';
  const patch = {};
  if (mean !== null && mean.trim() !== '') patch.mean = Number(mean);
  if (sd !== null && Number(sd) >= 0) patch.sd = Number(sd);
  if (sd === '-1') patch.sd = -1;
  patch.excluded = excludedStr === '1';
  await api('/api/markers/' + encodeURIComponent(id), {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(patch),
  });
  banner(`锦标 ${id} 已更新版本（重放只失效依赖它的分位数）`);
  await refresh();
}

async function editSegment(id) {
  const s = state.segments.find((x) => x.id === id);
  if (!s.present) {
    const g = prompt(`缺芯 ${id} 的最小年数（当前 ${s.gap_min_years}，必须 > 0）`, s.gap_min_years);
    if (g === null) return;
    await api('/api/segments/' + encodeURIComponent(id), {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ gap_min_years: Number(g) }),
    });
    banner(`缺芯 ${id} 边界已调整：增量重放将只失效依赖该缺芯的年龄分位数`);
  } else {
    const note = prompt('芯段备注', s.note);
    if (note === null) return;
    await api('/api/segments/' + encodeURIComponent(id), {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ note }),
    });
  }
  await refresh();
}

document.addEventListener('click', async (ev) => {
  const t = ev.target;
  try {
    if (t.id === 'btn-solve') {
      const body = await api('/api/solve', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ seed: Number($('seed').value), draws: Number($('draws').value) }),
      });
      await refresh();
      if (body.result.feasible) banner(`已生成运行 #${body.seq}（可行）`);
      else banner(`运行 #${body.seq} 被拒绝：${body.result.conflict.reason}`, 'error');
      showRun(body.seq);
    } else if (t.id === 'btn-rerun') {
      const base = Number($('base-run').value);
      const body = await api('/api/rerun', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ base_seq: base }),
      });
      await refresh();
      const r = body.report;
      banner(`#${body.seq} 增量重放：复用 ${r.reused_node_count}，失效 ${r.invalidated_node_count}`);
      showRun(body.seq);
    } else if (t.dataset.run) {
      showRun(Number(t.dataset.run));
    } else if (t.dataset.marker) {
      await editMarker(t.dataset.marker);
    } else if (t.dataset.segEdit || t.dataset.seg) {
      await editSegment(t.dataset.segEdit || t.dataset.seg);
    } else if (t.id === 'btn-reset') {
      if (!confirm('重置为固定 fixture（清空状态与运行记录）？')) return;
      const body = await api('/api/admin/reset', { method: 'POST' });
      await refresh();
      banner(`已重置，基线运行 #${body.baseline.seq}`);
      showRun(body.baseline.seq);
    } else if (t.id === 'btn-clear') {
      if (!confirm('清空整个数据库（状态与运行记录全删）？')) return;
      await api('/api/admin/clear', { method: 'POST' });
      await refresh();
      $('run-detail').innerHTML = '<p class="small">数据库已清空，可导入 Bundle 复核。</p>';
      banner('数据库已清空');
    } else if (t.id === 'btn-export') {
      const bundle = await api('/api/export');
      const blob = new Blob([JSON.stringify(bundle, null, 2)], { type: 'application/json' });
      const a = document.createElement('a');
      a.href = URL.createObjectURL(blob);
      a.download = 'hanceng-bundle.json';
      a.click();
    } else if (t.id === 'btn-import') {
      $('import-file').click();
    } else if (t.id === 'btn-diff') {
      const a = Number($('diff-a').value), b = Number($('diff-b').value);
      const d = await api(`/api/runs/${a}/diff/${b}`);
      const rows = d.nodes.map((n) => `<tr><td>${n.depth}</td><td>${fmt(n.median_delta)}</td><td>${fmt(n.q05_delta)}</td><td>${fmt(n.q95_delta)}</td></tr>`).join('');
      const erows = d.edges.map((e) => `<tr class="${e.is_gap ? 'gap-row' : ''}"><td>${e.depth_from}→${e.depth_to}</td><td>${fmt(e.rate_median_delta)}</td></tr>`).join('');
      $('diff-detail').innerHTML =
        `<div class="small">#${a} vs #${b}：节点最大中位差 ${d.max_abs_node_delta}，速率最大中位差 ${d.max_abs_edge_delta}</div>
         <table class="data"><thead><tr><th>深度</th><th>|Δ中位数|</th><th>|Δq05|</th><th>|Δq95|</th></tr></thead><tbody>${rows}</tbody></table>
         <table class="data" style="margin-top:6px"><thead><tr><th>深度段</th><th>|Δ速率中位数|</th></tr></thead><tbody>${erows}</tbody></table>`;
    }
  } catch (e) {
    banner(e.message, 'error');
  }
});

document.addEventListener('change', async (ev) => {
  const t = ev.target;
  try {
    if (t.dataset.exclude) {
      await api('/api/markers/' + encodeURIComponent(t.dataset.exclude), {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ excluded: t.checked }),
      });
      banner(`锦标 ${t.dataset.exclude} 已${t.checked ? '暂时排除' : '恢复参与'}`);
      await refresh();
    } else if (t.dataset.ex) {
      await api('/api/exchanges', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ group: t.dataset.ex, swapped: t.dataset.val === '1' }),
      });
      banner(`交换组 ${t.dataset.ex} 已${t.dataset.val === '1' ? '对调' : '正序'}`);
      await refresh();
    } else if (t.id === 'import-file') {
      const file = t.files[0];
      if (!file) return;
      const text = await file.text();
      const bundle = JSON.parse(text);
      const report = await api('/api/import', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(bundle),
      });
      await refresh();
      banner(`导入复核通过：${report.imported_runs} 条运行全部逐位复现`, '');
    }
  } catch (e) {
    banner(e.message, 'error');
  }
});

(async function init() {
  window.__segments = [];
  await refresh();
  window.__segments = state.segments;
  const feas = state.runs.filter((r) => r.status === 'feasible');
  if (feas.length) showRun(feas[0].seq);
})();
