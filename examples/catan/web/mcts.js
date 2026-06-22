// MCTS analysis panel: policy bar chart, search info, tree explorer.

class MCTSPanel {
  constructor() {
    this.barsEl = document.getElementById('policy-bars');
    this.simsEl = document.getElementById('sims-count');
    this.cpuLoadEl = document.getElementById('cpu-load-count');
    this.analysisBarEl = document.getElementById('analysis-bar-track');
    this.analysisP1El = document.getElementById('analysis-bar-p1');
    this.analysisP2El = document.getElementById('analysis-bar-p2');
    this.analysisP1LabelEl = document.getElementById('analysis-bar-p1-label');
    this.analysisP2LabelEl = document.getElementById('analysis-bar-p2-label');
    this.treeViewEl = document.getElementById('tree-view');
    this.onExplore = null;
    this.onPreview = null;
    this.onPreviewClear = null;
    this.expandedPaths = new Set();
  }

  // Update the analysis panel with a search snapshot.
  // Reuses existing DOM rows when the edge set hasn't changed to keep
  // click handlers stable during live search updates.
  updateSnapshot(snapshot, labels, currentPlayer = 0) {
    const pvDepth = snapshot.pv_depth ?? 0;
    this.simsEl.textContent = `${this._simSummary(snapshot)} - Depth ${pvDepth}`;
    this._updateAnalysisBar(snapshot.root_wdl);

    // Build sorted edge data
    const edges = snapshot.edges.map((e, i) => ({
      ...e,
      label: labels[i] || `Action ${e.action}`,
    }));

    // Sort by fresh visits from the current search. Reused total visits remain
    // visible, but they should not decide the recommended move.
    const qSign = currentPlayer === 0 ? 1 : -1;
    edges.sort((a, b) => {
      const af = this._freshVisits(a);
      const bf = this._freshVisits(b);
      const av = af > 0 ? 1 : 0;
      const bv = bf > 0 ? 1 : 0;
      if (av !== bv) return bv - av;
      if (av && bv) {
        if (af !== bf) return bf - af;
        const aq = (a.q ?? 0) * qSign;
        const bq = (b.q ?? 0) * qSign;
        return bq - aq;
      }
      const ap = a.improved_policy ?? 0;
      const bp = b.improved_policy ?? 0;
      return bp - ap;
    });

    // Check if the edge set changed (different actions or count).
    const newKey = edges.map(e => e.action).join(',');
    const rebuild = newKey !== this._lastEdgeKey;
    this._lastEdgeKey = newKey;

    if (rebuild) {
      this.barsEl.innerHTML = '';
    }

    const maxPolicy = Math.max(...edges.map(e => e.improved_policy ?? 0), 0.001);

    for (let idx = 0; idx < edges.length; idx++) {
      const edge = edges[idx];
      const policy = edge.improved_policy ?? 0;
      const pct = (policy / maxPolicy) * 100;
      const q = edge.q;
      const qColor = q != null ? this._qColor(q) : '#666';

      if (rebuild) {
        const row = document.createElement('div');
        row.className = 'flex items-center gap-1 px-1.5 py-0.5 text-[11px] cursor-pointer rounded hover:bg-bg';
        row.dataset.action = edge.action;
        row.addEventListener('click', () => {
          this.onExplore?.([edge.action]);
        });
        row.addEventListener('mouseenter', () => {
          this.onPreview?.(edge.action);
        });
        row.addEventListener('mouseleave', () => {
          this.onPreviewClear?.();
        });
        row.addEventListener('focus', () => {
          this.onPreview?.(edge.action);
        });
        row.addEventListener('blur', () => {
          this.onPreviewClear?.();
        });
        row.tabIndex = 0;

        const rankColor = this._rankHighlightColor(idx);
        row.innerHTML = `
          <span data-role="rank" class="w-5 shrink-0 text-right text-gray-500 text-[10px]" style="${rankColor ? `color:${rankColor};font-weight:700` : ''}">${idx + 1}</span>
          <span data-role="label" class="w-32 shrink-0 overflow-hidden text-ellipsis whitespace-nowrap" title="${edge.label}">${edge.label}</span>
          <div class="flex-1 h-3.5 bg-bar rounded-sm relative">
            <div class="h-full rounded-sm transition-[width] duration-150" style="width:${pct}%;background:${qColor}"></div>
          </div>
          <span data-role="visits" class="w-16 text-right text-gray-500 shrink-0 text-[10px]" title="Fresh / total simulations">${this._visitLabel(edge)}</span>
          <span data-role="q" class="w-11 text-right shrink-0 text-[10px]" style="color:${qColor}">${q != null ? ((q + 1) / 2 * 100).toFixed(0) + '%' : '—'}</span>
          <span data-role="depth" class="w-6 text-right text-gray-500 shrink-0 text-[10px]">${edge.depth || ''}</span>
        `;

        this.barsEl.appendChild(row);
      } else {
        // In-place update: just patch the changing values.
        const row = this.barsEl.children[idx];
        if (!row) continue;
        const bar = row.querySelector('.bg-bar > div');
        if (bar) {
          bar.style.width = `${pct}%`;
          bar.style.background = qColor;
        }
        const rank = row.querySelector('[data-role="rank"]');
        const visits = row.querySelector('[data-role="visits"]');
        const qEl = row.querySelector('[data-role="q"]');
        const depth = row.querySelector('[data-role="depth"]');
        if (rank) {
          const rankColor = this._rankHighlightColor(idx);
          rank.textContent = `${idx + 1}`;
          rank.style.color = rankColor || '';
          rank.style.fontWeight = rankColor ? '700' : '';
        }
        if (visits) {
          visits.textContent = this._visitLabel(edge);
          visits.title = 'Fresh / total simulations';
        }
        if (qEl) {
          qEl.textContent = q != null ? ((q + 1) / 2 * 100).toFixed(0) + '%' : '—';
          qEl.style.color = qColor;
        }
        if (depth) depth.textContent = `${edge.depth || ''}`;
      }
    }
  }

  // Display a subtree in the tree explorer.
  // If the tree structure matches the current DOM, update values in-place
  // to keep click handlers and expanded state stable.
  showSubtree(tree) {
    if (this.treeViewEl.firstChild && this._updateNodeInPlace(this.treeViewEl.firstChild, tree)) {
      return; // in-place update succeeded
    }
    this.treeViewEl.innerHTML = '';
    this.treeViewEl.appendChild(this._renderNode(tree, 0, true, []));
  }

  // Try to patch an existing tree DOM node with new data.
  // Returns true if structure matched and values were updated in-place.
  _updateNodeInPlace(domNode, dataNode) {
    const header = domNode.querySelector(':scope > div:not(.tree-children)');
    if (!header) return false;

    // Check structure match: same action
    const existingAction = header.dataset.nodeAction;
    const newAction = dataNode.action != null ? String(dataNode.action) : 'root';
    if (existingAction !== newAction) return false;

    // Update header text
    const playerPrefix = dataNode.player != null ? `P${dataNode.player + 1}: ` : '';
    const actionLabel = dataNode.label || (dataNode.action != null ? `Action ${dataNode.action}` : 'Root');
    const q = (dataNode.wdl[0] - dataNode.wdl[2]).toFixed(3);
    const branch = header.dataset.branch || '';
    const prefix = header.dataset.prefix || '';
    header.textContent = `${prefix}${branch}${playerPrefix}${actionLabel} [${dataNode.kind}] V:${dataNode.visits} Q:${q}`;

    // Match children by action. If the sets differ (new children not in
    // DOM, or DOM children absent from new data), bail out so the caller
    // does a full rebuild — otherwise stale children from a previous
    // navigation would remain visible.
    const childrenDiv = domNode.querySelector(':scope > .tree-children');
    const newChildren = dataNode.children || [];
    const domChildren = childrenDiv ? [...childrenDiv.children] : [];
    if (newChildren.length !== domChildren.length) return false;
    const childDomMap = {};
    for (const child of domChildren) {
      const ch = child.querySelector(':scope > div:not(.tree-children)');
      if (ch?.dataset.nodeAction) childDomMap[ch.dataset.nodeAction] = child;
    }
    for (const childData of newChildren) {
      const key = childData.action != null ? String(childData.action) : 'root';
      if (!childDomMap[key]) return false; // structural change
    }
    for (const childData of newChildren) {
      const key = childData.action != null ? String(childData.action) : 'root';
      if (!this._updateNodeInPlace(childDomMap[key], childData)) return false;
    }
    return true;
  }

  showProgress(snapshot, budget, simsTotal, cpuLoad = null) {
    this.updateCpuLoad(cpuLoad);
    if (budget && budget.mode === 'pv_depth') {
      const depth = snapshot?.pv_depth ?? 0;
      const done = snapshot?.fresh_simulations ?? snapshot?.total_simulations ?? 0;
      this.simsEl.textContent = `Depth ${depth} / ${budget.value} - ${done} sims`;
      return;
    }
    const done = snapshot?.fresh_simulations ?? snapshot?.total_simulations ?? 0;
    const total = budget?.value ?? simsTotal;
    this.simsEl.textContent = `${done} / ${total} sims`;
  }

  clear() {
    this._lastEdgeKey = null;
    this.expandedPaths.clear();
    this.barsEl.innerHTML = '';
    this.simsEl.textContent = '0 sims';
    this.updateCpuLoad(0);
    this.treeViewEl.innerHTML = '';
    this._updateAnalysisBar([0, 1, 0]);
  }

  updateCpuLoad(cpuLoad) {
    if (!this.cpuLoadEl) return;
    const value = Number(cpuLoad);
    if (!Number.isFinite(value)) return;
    const pct = Math.max(0, Math.min(100, Math.round(value)));
    this.cpuLoadEl.textContent = `Load ${pct}%`;
    this.cpuLoadEl.title = `Server CPU load ${pct}%`;
  }

  updateAnalysisBar(rootWdl) {
    this._updateAnalysisBar(rootWdl);
  }

  _updateAnalysisBar(rootWdl) {
    if (!this.analysisBarEl || !this.analysisP1El || !this.analysisP2El) return;
    let [w, d, l] = Array.isArray(rootWdl) ? rootWdl : [0, 1, 0];
    w = this._clamp01(w);
    d = this._clamp01(d);
    l = this._clamp01(l);

    const total = w + d + l;
    if (total > 0) {
      w /= total;
      d /= total;
      l /= total;
    } else {
      w = 0;
      d = 1;
      l = 0;
    }

    const p1Share = this._clamp01(w + d / 2);
    const value = Math.round(p1Share * 100);
    const p2Value = 100 - value;
    const p1Percent = value;
    const p2Percent = p2Value;
    const wPct = Math.round(w * 100);
    const dPct = Math.round(d * 100);
    const lPct = Math.round(l * 100);

    this.analysisP1El.style.height = `${p1Percent}%`;
    this.analysisP2El.style.height = `${p2Percent}%`;
    if (this.analysisP1LabelEl) this.analysisP1LabelEl.textContent = `${value}`;
    if (this.analysisP2LabelEl) this.analysisP2LabelEl.textContent = `${p2Value}`;
    this.analysisBarEl.setAttribute('aria-valuenow', `${value}`);
    this.analysisBarEl.setAttribute('aria-valuetext', `P1 ${value} percent, P2 ${p2Value} percent, P1 win ${wPct} percent, draw ${dPct} percent, P2 win ${lPct} percent`);
    this.analysisBarEl.title = `P1 ${value} percent, P2 ${p2Value} percent; P1 win ${wPct} percent, draw ${dPct} percent, P2 win ${lPct} percent`;
  }

  _renderNode(node, depth, isLast = true, path = [], prefix = '') {
    const div = document.createElement('div');
    const pathKey = path.join(',');

    const header = document.createElement('div');
    header.className = 'cursor-pointer py-0.5 whitespace-nowrap hover:text-accent';
    header.dataset.nodeAction = node.action != null ? String(node.action) : 'root';

    const playerPrefix = node.player != null ? `P${node.player + 1}: ` : '';
    const actionLabel = node.label || (node.action != null ? `Action ${node.action}` : 'Root');
    const visits = node.visits;
    const q = (node.wdl[0] - node.wdl[2]).toFixed(3);
    const kind = node.kind;

    const branch = depth === 0 ? '' : (isLast ? '└ ' : '├ ');
    header.dataset.branch = branch;
    header.dataset.prefix = prefix;
    header.textContent = `${prefix}${branch}${playerPrefix}${actionLabel} [${kind}] V:${visits} Q:${q}`;
    div.appendChild(header);

    if (node.children && node.children.length > 0) {
      const childrenDiv = document.createElement('div');
      childrenDiv.className = 'tree-children';
      if (this.expandedPaths.has(pathKey)) {
        childrenDiv.classList.add('open');
      }

      header.addEventListener('click', () => {
        childrenDiv.classList.toggle('open');
        if (childrenDiv.classList.contains('open')) {
          this.expandedPaths.add(pathKey);
        } else {
          this.expandedPaths.delete(pathKey);
        }
      });

      const sorted = [...node.children].sort((a, b) => b.visits - a.visits);
      for (let i = 0; i < sorted.length; i++) {
        const childPath = [...path, sorted[i].action];
        const childPrefix = prefix + (depth === 0 ? '' : (isLast ? '  ' : '│ '));
        childrenDiv.appendChild(this._renderNode(sorted[i], depth + 1, i === sorted.length - 1, childPath, childPrefix));
      }
      div.appendChild(childrenDiv);
    }

    return div;
  }

  // Color Q values: green for positive (good for P1), red for negative.
  _rankHighlightColor(index) {
    return ['#1b8a2a', '#1a6fc4', '#b82040'][index] || '';
  }

  _qColor(q) {
    if (q > 0.1) return '#4caf50';
    if (q > 0) return '#8bc34a';
    if (q > -0.1) return '#ff9800';
    return '#f44336';
  }

  _freshVisits(edge) {
    return Number.isFinite(edge?.fresh_visits) ? edge.fresh_visits : (edge?.visits ?? 0);
  }

  _totalVisits(edge) {
    return Number.isFinite(edge?.visits) ? edge.visits : this._freshVisits(edge);
  }

  _visitLabel(edge) {
    const fresh = this._freshVisits(edge);
    const total = this._totalVisits(edge);
    return total > fresh ? `${fresh}/${total}` : `${fresh}`;
  }

  _simSummary(snapshot) {
    const fresh = snapshot?.fresh_simulations ?? snapshot?.total_simulations ?? 0;
    const total = snapshot?.total_simulations ?? fresh;
    return total > fresh ? `${fresh} fresh / ${total} total sims` : `${fresh} sims`;
  }

  _clamp01(value) {
    if (!Number.isFinite(value)) return 0;
    return Math.max(0, Math.min(1, value));
  }
}
