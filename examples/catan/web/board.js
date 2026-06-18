// SVG hex board rendering for Catan.
//
// Renders tiles, buildings, roads, robber, ports, and legal-action overlays.

const TERRAIN_COLORS = {
  forest: '#2d5a27',
  hills: '#b85c38',
  pasture: '#7ec850',
  fields: '#e8b430',
  mountains: '#7a7a7a',
  desert: '#d4c088',
};

const HEX_SIZE = 50;
const SQRT3 = Math.sqrt(3);
const BUILDING_SCALE = 1.5;
const SETTLEMENT_SIZE = 10 * BUILDING_SCALE;
const SETTLEMENT_HALF = SETTLEMENT_SIZE / 2;
const SETTLEMENT_ACTION_RADIUS = 8 * BUILDING_SCALE;
const SETTLEMENT_HIGHLIGHT_RADIUS = 9 * BUILDING_SCALE;
const CITY_ACTION_RADIUS = 10 * BUILDING_SCALE;
const CITY_HIGHLIGHT_HALF = 8 * BUILDING_SCALE;
const CITY_HIGHLIGHT_SIZE = CITY_HIGHLIGHT_HALF * 2;
const CITY_HIGHLIGHT_RX = 2 * BUILDING_SCALE;

function catanPips(number) {
  return number === 7 ? 0 : Math.max(0, 6 - Math.abs(7 - number));
}

class Board {
  constructor(svgEl) {
    this.svg = svgEl;
    this.boardData = null;
    this.onActionClick = null;
    this.onTileClick = null;
    this.rotationStep = 0;
    this.mirrored = false;
    this.boardCenter = [0, 0];
    this.contentGroup = null;
  }

  rotate(delta) {
    this.setRotation(this.rotationStep + delta);
  }

  rotateClockwise() {
    this.rotate(this.mirrored ? -1 : 1);
  }

  rotateCounterclockwise() {
    this.rotate(this.mirrored ? 1 : -1);
  }

  setRotation(step) {
    this.rotationStep = ((step % 6) + 6) % 6;
    this._applyRotation();
  }

  toggleMirror() {
    this.setMirrored(!this.mirrored);
    return this.mirrored;
  }

  setMirrored(mirrored) {
    this.mirrored = Boolean(mirrored);
    this._applyRotation();
  }

  // Initial render from board topology (tiles, nodes, edges, ports).
  initBoard(board) {
    this.boardData = board;
    this._centroid = null;
    this.svg.innerHTML = '';

    // Compute viewBox from node positions
    let minX = Infinity, minY = Infinity, maxX = -Infinity, maxY = -Infinity;
    for (const [x, y] of board.nodes) {
      minX = Math.min(minX, x); minY = Math.min(minY, y);
      maxX = Math.max(maxX, x); maxY = Math.max(maxY, y);
    }
    const pad = 40;
    const centerX = (minX + maxX) / 2;
    const centerY = (minY + maxY) / 2;
    let radius = 0;
    for (const [x, y] of board.nodes) {
      radius = Math.max(radius, Math.hypot(x - centerX, y - centerY));
    }
    const viewX = centerX - radius - pad;
    const viewY = centerY - radius - pad;
    const viewW = (radius + pad) * 2;
    const viewH = viewW;
    this.boardCenter = [centerX, centerY];
    this.svg.setAttribute('viewBox',
      `${viewX} ${viewY} ${viewW} ${viewH}`);

    // Ocean background
    this.svg.appendChild(this._el('rect', {
      x: viewX, y: viewY,
      width: viewW, height: viewH,
      fill: '#1a5276', rx: 10
    }));

    this.contentGroup = this._el('g', { class: 'board-content' });
    this.svg.appendChild(this.contentGroup);

    // Draw tiles
    const tilesG = this._g('tiles');
    for (const tile of board.tiles) {
      this._drawTile(tilesG, tile);
    }

    // Draw edges (roads layer - empty initially)
    this._g('roads');

    // Draw ports
    const portsG = this._g('ports');
    for (const port of board.ports) {
      this._drawPort(portsG, port, board.nodes);
    }

    // Draw nodes (buildings layer - empty initially)
    this._g('buildings');

    // Robber layer
    this._g('robber');

    // Persistent hover targets for tiles, edges, and nodes
    const hitsG = this._g('hit-targets');
    for (let tid = 0; tid < board.tiles.length; tid++) {
      const tile = board.tiles[tid];
      if (!tile) continue;
      const el = this.onTileClick
        ? this._el('polygon', {
          points: this._hexPoints(tile.cx, tile.cy, HEX_SIZE),
          fill: 'transparent', 'pointer-events': 'all'
        })
        : this._el('circle', {
          cx: tile.cx, cy: tile.cy, r: 18,
          fill: 'transparent', 'pointer-events': 'all'
        });
      this._attachTooltip(el, `T${tid}`);
      if (this.onTileClick) {
        el.setAttribute('cursor', 'pointer');
        el.addEventListener('click', () => this.onTileClick?.(tid));
      }
      hitsG.appendChild(el);
    }
    if (!this.onTileClick) {
      for (let eid = 0; eid < board.edges.length; eid++) {
        const edge = board.edges[eid];
        if (!edge) continue;
        const [n0, n1] = edge;
        const [x0, y0] = board.nodes[n0];
        const [x1, y1] = board.nodes[n1];
        const el = this._el('line', {
          x1: x0, y1: y0, x2: x1, y2: y1,
          stroke: 'transparent', 'stroke-width': 6,
          'stroke-linecap': 'round', 'pointer-events': 'stroke'
        });
        this._attachTooltip(el, `E${eid}`);
        hitsG.appendChild(el);
      }
      for (let nid = 0; nid < board.nodes.length; nid++) {
        const [x, y] = board.nodes[nid];
        const el = this._el('circle', {
          cx: x, cy: y, r: 7,
          fill: 'transparent', 'pointer-events': 'all'
        });
        this._attachTooltip(el, `N${nid}`);
        hitsG.appendChild(el);
      }
    }

    // Overlay for legal actions
    this._g('overlays');

    // Transient hover preview for legal moves
    const previewG = this._g('move-preview');
    previewG.setAttribute('pointer-events', 'none');
    previewG.setAttribute('opacity', '0.6');

    this._applyRotation();
  }

  // Render a local editor draft using normal board geometry.
  renderEditorBoard(board) {
    this.initBoard(board);
    this.clearOverlays();
  }

  // Update dynamic elements from a frame.
  updateFrame(frame, board) {
    if (!this.boardData) return;
    const nodes = board.nodes;

    // Roads
    const roadsG = this.svg.querySelector('.roads');
    roadsG.innerHTML = '';
    for (let p = 0; p < 2; p++) {
      const color = p === 0 ? '#4a9eff' : '#ff6b6b';
      for (const eid of frame.buildings[p].roads) {
        const edge = board.edges[eid];
        if (!edge) continue;
        const [n0, n1] = edge;
        const [x0, y0] = nodes[n0];
        const [x1, y1] = nodes[n1];
        const line = this._el('line', {
          x1: x0, y1: y0, x2: x1, y2: y1,
          stroke: color, 'stroke-width': 4, 'stroke-linecap': 'round'
        });
        roadsG.appendChild(line);
      }
    }

    // Buildings
    const buildG = this.svg.querySelector('.buildings');
    buildG.innerHTML = '';
    for (let p = 0; p < 2; p++) {
      const color = p === 0 ? '#4a9eff' : '#ff6b6b';
      for (const nid of frame.buildings[p].settlements) {
        const [x, y] = nodes[nid];
        const settlement = this._el('rect', {
          x: x - SETTLEMENT_HALF, y: y - SETTLEMENT_HALF,
          width: SETTLEMENT_SIZE, height: SETTLEMENT_SIZE,
          fill: color, stroke: '#111', 'stroke-width': 1
        });
        buildG.appendChild(this._keepUpright(settlement, x, y));
      }
      for (const nid of frame.buildings[p].cities) {
        const [x, y] = nodes[nid];
        const city = this._el('polygon', {
          points: this._cityPoints(x, y),
          fill: color, stroke: '#111', 'stroke-width': 1
        });
        buildG.appendChild(this._keepUpright(city, x, y));
      }
    }

    // Robber
    const robberG = this.svg.querySelector('.robber');
    robberG.innerHTML = '';
    if (board.tiles[frame.robber]) {
      const tile = board.tiles[frame.robber];
      robberG.appendChild(this._el('circle', {
        cx: tile.cx, cy: tile.cy - 18, r: 8,
        fill: '#111', stroke: '#e94560', 'stroke-width': 2
      }));
    }

    this._applyRotation();
  }

  // Show legal action overlays on the board.
  showLegalActions(actions, board, playerIndex = 0) {
    const overlayG = this.svg.querySelector('.overlays');
    overlayG.innerHTML = '';
    if (!board) return;

    for (const { action, label } of actions) {
      const overlay = this._actionOverlay(action, label, board, playerIndex);
      if (overlay) overlayG.appendChild(overlay);
    }
  }

  clearOverlays() {
    const overlayG = this.svg.querySelector('.overlays');
    if (overlayG) overlayG.innerHTML = '';
    this.clearActionPreview();
    this.clearSearchHighlights();
  }

  showActionPreview(action, board, playerIndex = 0) {
    const previewG = this.svg.querySelector('.move-preview');
    if (!previewG) return;
    previewG.innerHTML = '';
    if (!board) return;

    const nodes = board.nodes;
    const color = this._playerColor(playerIndex);
    let el = null;
    let uprightAt = null;

    // Settlement: action 0..54 -> node
    if (action < 54) {
      const [x, y] = nodes[action];
      el = this._el('rect', {
        x: x - SETTLEMENT_HALF, y: y - SETTLEMENT_HALF,
        width: SETTLEMENT_SIZE, height: SETTLEMENT_SIZE,
        fill: color, stroke: '#fff', 'stroke-width': 1.5
      });
      uprightAt = [x, y];
    }
    // Road: 54..126 -> edge
    else if (action >= 54 && action < 126) {
      const eid = action - 54;
      const edge = board.edges[eid];
      if (edge) {
        const [x0, y0] = nodes[edge[0]];
        const [x1, y1] = nodes[edge[1]];
        el = this._el('line', {
          x1: x0, y1: y0, x2: x1, y2: y1,
          stroke: color, 'stroke-width': 6, 'stroke-linecap': 'round'
        });
      }
    }
    // City: 126..180 -> node
    else if (action >= 126 && action < 180) {
      const nid = action - 126;
      const [x, y] = nodes[nid];
      el = this._el('polygon', {
        points: this._cityPoints(x, y),
        fill: color, stroke: '#fff', 'stroke-width': 1.5
      });
      uprightAt = [x, y];
    }
    // Robber: 205..224 -> tile
    else if (action >= 205 && action < 224) {
      const tid = action - 205;
      const tile = board.tiles[tid];
      if (tile) {
        el = this._el('circle', {
          cx: tile.cx, cy: tile.cy - 18, r: 8,
          fill: '#111', stroke: '#fff', 'stroke-width': 2.5
        });
      }
    }

    if (!el) return;
    previewG.appendChild(uprightAt ? this._keepUpright(el, uprightAt[0], uprightAt[1]) : el);
    this._applyRotation();
  }

  clearActionPreview() {
    const previewG = this.svg.querySelector('.move-preview');
    if (previewG) previewG.innerHTML = '';
  }

  // Highlight top spatial actions from search on the board.
  // edges: sorted array of { action, improved_policy, visits, q }
  showSearchHighlights(edges, board) {
    this.clearSearchHighlights();
    if (!board) return;
    const g = this.svg.querySelector('.overlays');
    if (!g) return;

    const RANK_COLORS = ['#1b8a2a', '#1a6fc4', '#b82040']; // dark green, dark blue, dark red
    const nodes = board.nodes;
    let rank = 0;

    for (const edge of edges) {
      if (rank >= 3) break;
      const a = edge.action;
      const color = RANK_COLORS[rank];
      const freshVisits = Number.isFinite(edge.fresh_visits)
        ? edge.fresh_visits
        : (Number.isFinite(edge.visits) ? edge.visits : 0);
      const totalVisits = Number.isFinite(edge.visits) ? edge.visits : freshVisits;
      const visitsLabel = totalVisits > freshVisits ? `${freshVisits}/${totalVisits}` : `${freshVisits}`;
      const label = `#${rank + 1}: ${edge.label} (${visitsLabel} visits)`;
      let el = null;

      // Settlement
      if (a < 54) {
        const [x, y] = nodes[a];
        el = this._el('circle', {
          cx: x, cy: y, r: SETTLEMENT_HIGHLIGHT_RADIUS,
          fill: 'none', stroke: color,
          'stroke-width': 2.5, 'pointer-events': 'none',
          class: 'search-highlight', opacity: 0.9,
        });
      }
      // Road
      else if (a >= 54 && a < 126) {
        const eid = a - 54;
        const e = board.edges[eid];
        if (e) {
          const [x0, y0] = nodes[e[0]];
          const [x1, y1] = nodes[e[1]];
          el = this._el('line', {
            x1: x0, y1: y0, x2: x1, y2: y1,
            stroke: color, 'stroke-width': 5, 'stroke-linecap': 'round',
            'pointer-events': 'none', class: 'search-highlight', opacity: 0.8,
          });
        }
      }
      // City
      else if (a >= 126 && a < 180) {
        const nid = a - 126;
        const [x, y] = nodes[nid];
        el = this._el('rect', {
          x: x - CITY_HIGHLIGHT_HALF, y: y - CITY_HIGHLIGHT_HALF,
          width: CITY_HIGHLIGHT_SIZE, height: CITY_HIGHLIGHT_SIZE, rx: CITY_HIGHLIGHT_RX,
          fill: 'none', stroke: color,
          'stroke-width': 2.5, 'pointer-events': 'none',
          class: 'search-highlight', opacity: 0.9,
        });
      }
      // Robber
      else if (a >= 205 && a < 224) {
        const tid = a - 205;
        const tile = board.tiles[tid];
        if (tile) {
          el = this._el('circle', {
            cx: tile.cx, cy: tile.cy, r: 18,
            fill: 'none', stroke: color,
            'stroke-width': 4, 'pointer-events': 'none',
            class: 'search-highlight', opacity: 0.9,
          });
        }
      }

      if (el) {
        this._attachTooltip(el, label);
        el.style.pointerEvents = 'auto';
        g.appendChild(el);
        rank++;
      }
    }
  }

  clearSearchHighlights() {
    for (const el of this.svg.querySelectorAll('.search-highlight')) {
      el.remove();
    }
  }

  _applyRotation() {
    if (!this.contentGroup) return;
    const angle = this.rotationStep * 60;
    const radians = angle * Math.PI / 180;
    const cos = Math.cos(radians);
    const sin = Math.sin(radians);
    const [cx, cy] = this.boardCenter;
    const a = this.mirrored ? -cos : cos;
    const b = sin;
    const c = this.mirrored ? sin : -sin;
    const d = cos;
    const e = cx - a * cx - c * cy;
    const f = cy - b * cx - d * cy;
    this.contentGroup.setAttribute('transform', `matrix(${a} ${b} ${c} ${d} ${e} ${f})`);

    const det = a * d - b * c;
    const ia = d / det;
    const ib = -b / det;
    const ic = -c / det;
    const id = a / det;
    for (const el of this.contentGroup.querySelectorAll('.board-upright')) {
      const ux = parseFloat(el.dataset.uprightX);
      const uy = parseFloat(el.dataset.uprightY);
      const ie = ux - ia * ux - ic * uy;
      const if_ = uy - ib * ux - id * uy;
      el.setAttribute('transform', `matrix(${ia} ${ib} ${ic} ${id} ${ie} ${if_})`);
    }
  }

  _keepUpright(el, x, y) {
    el.classList.add('board-upright');
    el.dataset.uprightX = x;
    el.dataset.uprightY = y;
    return el;
  }

  boardPointToShellPoint(x, y) {
    const shell = document.getElementById('board-shell');
    const matrix = this.contentGroup?.getScreenCTM?.() || this.svg?.getScreenCTM?.();
    if (!shell || !matrix) return null;

    let screenPoint;
    if (typeof this.svg.createSVGPoint === 'function') {
      const point = this.svg.createSVGPoint();
      point.x = x;
      point.y = y;
      screenPoint = point.matrixTransform(matrix);
    } else if (typeof DOMPoint === 'function') {
      screenPoint = new DOMPoint(x, y).matrixTransform(matrix);
    } else {
      return null;
    }

    const shellRect = shell.getBoundingClientRect();
    return {
      x: screenPoint.x - shellRect.left,
      y: screenPoint.y - shellRect.top,
    };
  }

  // Attach instant tooltip (replaces slow browser-native <title>).
  _attachTooltip(el, label) {
    const tip = document.getElementById('svg-tooltip');
    el.addEventListener('mouseenter', (e) => {
      tip.textContent = label;
      tip.style.display = 'block';
      tip.style.left = e.clientX + 10 + 'px';
      tip.style.top = e.clientY + 10 + 'px';
    });
    el.addEventListener('mousemove', (e) => {
      tip.style.left = e.clientX + 10 + 'px';
      tip.style.top = e.clientY + 10 + 'px';
    });
    el.addEventListener('mouseleave', () => {
      tip.style.display = 'none';
    });
  }

  // Create a clickable overlay element for an action.
  _actionOverlay(action, label, board, playerIndex = 0) {
    const nodes = board.nodes;
    // Settlement: action 0..54 -> node
    if (action < 54) {
      const [x, y] = nodes[action];
      const el = this._el('circle', {
        cx: x, cy: y, r: SETTLEMENT_ACTION_RADIUS,
        fill: 'rgba(255,255,255,0.15)', stroke: 'rgba(255,255,255,0.5)',
        'stroke-width': 1.5, cursor: 'pointer', class: 'action-overlay'
      });
      el.dataset.action = action;
      el.addEventListener('click', () => this.onActionClick?.(action));
      el.addEventListener('mouseenter', () => this.showActionPreview(action, board, playerIndex));
      el.addEventListener('mouseleave', () => this.clearActionPreview());
      this._attachTooltip(el, label);
      return el;
    }
    // Road: 54..126 -> edge
    if (action >= 54 && action < 126) {
      const eid = action - 54;
      const edge = board.edges[eid];
      if (!edge) return null;
      const [n0, n1] = edge;
      const [x0, y0] = nodes[n0];
      const [x1, y1] = nodes[n1];
      const el = this._el('line', {
        x1: x0, y1: y0, x2: x1, y2: y1,
        stroke: 'rgba(255,255,255,0.4)', 'stroke-width': 5,
        'stroke-linecap': 'round', cursor: 'pointer', class: 'action-overlay'
      });
      el.dataset.action = action;
      el.addEventListener('click', () => this.onActionClick?.(action));
      el.addEventListener('mouseenter', () => this.showActionPreview(action, board, playerIndex));
      el.addEventListener('mouseleave', () => this.clearActionPreview());
      this._attachTooltip(el, label);
      return el;
    }
    // City: 126..180 -> node
    if (action >= 126 && action < 180) {
      const nid = action - 126;
      const [x, y] = nodes[nid];
      const el = this._el('circle', {
        cx: x, cy: y, r: CITY_ACTION_RADIUS,
        fill: 'rgba(255,255,255,0.15)', stroke: 'rgba(255,255,255,0.5)',
        'stroke-width': 1.5, cursor: 'pointer', class: 'action-overlay'
      });
      el.dataset.action = action;
      el.addEventListener('click', () => this.onActionClick?.(action));
      el.addEventListener('mouseenter', () => this.showActionPreview(action, board, playerIndex));
      el.addEventListener('mouseleave', () => this.clearActionPreview());
      this._attachTooltip(el, label);
      return el;
    }
    // Robber: 205..224 -> tile
    if (action >= 205 && action < 224) {
      const tid = action - 205;
      const tile = board.tiles[tid];
      if (!tile) return null;
      const el = this._el('circle', {
        cx: tile.cx, cy: tile.cy, r: 15,
        fill: 'rgba(255,255,255,0.15)', stroke: 'rgba(255,255,255,0.5)',
        'stroke-width': 1.5, cursor: 'pointer', class: 'action-overlay'
      });
      el.dataset.action = action;
      el.addEventListener('click', () => this.onActionClick?.(action));
      el.addEventListener('mouseenter', () => this.showActionPreview(action, board, playerIndex));
      el.addEventListener('mouseleave', () => this.clearActionPreview());
      this._attachTooltip(el, label);
      return el;
    }
    return null;
  }

  _drawTile(parent, tile) {
    const { cx, cy, terrain, number } = tile;
    const color = TERRAIN_COLORS[terrain] || '#263044';

    const attrs = {
      points: this._hexPoints(cx, cy, HEX_SIZE),
      fill: color,
      stroke: tile.selected ? '#e94560' : (terrain ? '#111' : '#637089'),
      'stroke-width': tile.selected ? 3 : 1
    };
    if (!terrain) attrs['stroke-dasharray'] = '5 4';
    parent.appendChild(this._el('polygon', attrs));

    // Number token
    if (number) {
      const isRed = number === 6 || number === 8;
      const tokenG = this._el('g', {});
      tokenG.appendChild(this._el('circle', {
        cx, cy, r: 12,
        fill: '#f5f0e1', stroke: '#333', 'stroke-width': 0.5
      }));
      const txt = this._el('text', {
        x: cx, y: cy + 1.5,
        'text-anchor': 'middle', 'font-size': '12',
        'font-weight': isRed ? 'bold' : 'normal',
        fill: isRed ? '#c00' : '#333'
      });
      txt.textContent = number;
      tokenG.appendChild(txt);

      const pipCount = catanPips(number);
      const pipSpacing = 3.2;
      const pipStart = cx - ((pipCount - 1) * pipSpacing) / 2;
      for (let i = 0; i < pipCount; i++) {
        tokenG.appendChild(this._el('circle', {
          cx: pipStart + i * pipSpacing, cy: cy + 7.5, r: 1.05,
          fill: isRed ? '#c00' : '#333'
        }));
      }

      parent.appendChild(this._keepUpright(tokenG, cx, cy));
    }
  }

  _drawPort(parent, port, nodes) {
    const PORT_COLORS = {
      lumber: '#2d5a27', brick: '#b85c38', wool: '#7ec850',
      grain: '#e8b430', ore: '#7a7a7a', generic: '#ffffff',
    };
    const [n0, n1] = port.nodes;
    const [x0, y0] = nodes[n0];
    const [x1, y1] = nodes[n1];
    const mx = (x0 + x1) / 2;
    const my = (y0 + y1) / 2;
    const color = PORT_COLORS[port.kind] || PORT_COLORS.generic;
    const ratio = port.kind === 'generic' ? '3:1' : '2:1';
    const darkText = port.kind === 'grain' || port.kind === 'wool' || port.kind === 'generic';

    // Normal perpendicular to the port edge, pointing outward
    if (!this._centroid) {
      let sx = 0, sy = 0;
      for (const [x, y] of nodes) { sx += x; sy += y; }
      this._centroid = [sx / nodes.length, sy / nodes.length];
    }
    const [cx, cy] = this._centroid;
    // Edge direction and its perpendicular
    const ex = x1 - x0, ey = y1 - y0;
    let nx = -ey, ny = ex;
    // Pick the normal pointing away from board center
    const toCenterX = cx - mx, toCenterY = cy - my;
    if (nx * toCenterX + ny * toCenterY > 0) { nx = -nx; ny = -ny; }
    const nlen = Math.sqrt(nx * nx + ny * ny) || 1;
    const offset = 18;
    const lx = mx + nx / nlen * offset;
    const ly = my + ny / nlen * offset;

    parent.appendChild(this._el('path', {
      d: `M ${x0} ${y0} L ${lx} ${ly} L ${x1} ${y1}`,
      fill: 'none',
      stroke: '#d2b48c',
      'stroke-width': 2,
      'stroke-linecap': 'round',
      'stroke-linejoin': 'round',
      opacity: 0.85,
      'pointer-events': 'none',
    }));

    // Circle label with ratio
    const isGeneric = port.kind === 'generic';
    parent.appendChild(this._el('circle', {
      cx: lx, cy: ly, r: 10,
      fill: isGeneric ? '#334' : color,
      stroke: isGeneric ? color : 'none', 'stroke-width': 1.5,
      opacity: 0.85
    }));
    const txt = this._el('text', {
      x: lx, y: ly + 3,
      'text-anchor': 'middle', 'font-size': '8',
      fill: isGeneric ? '#ddd' : (darkText ? '#222' : '#fff'),
      'font-weight': '600'
    });
    txt.textContent = ratio;
    parent.appendChild(this._keepUpright(txt, lx, ly));
  }

  _playerColor(playerIndex) {
    return playerIndex === 0 ? '#4a9eff' : '#ff6b6b';
  }

  _cityPoints(x, y) {
    const top = 9 * BUILDING_SCALE;
    const side = 7 * BUILDING_SCALE;
    const roof = 3 * BUILDING_SCALE;
    return `${x},${y - top} ${x + side},${y - roof} ${x + side},${y + side} ${x - side},${y + side} ${x - side},${y - roof}`;
  }

  _hexPoints(cx, cy, size) {
    let points = '';
    for (let i = 0; i < 6; i++) {
      const angle = (Math.PI / 3) * i - Math.PI / 6;
      const px = cx + size * Math.cos(angle);
      const py = cy + size * Math.sin(angle);
      points += `${px},${py} `;
    }
    return points.trim();
  }

  _g(cls) {
    const g = document.createElementNS('http://www.w3.org/2000/svg', 'g');
    g.setAttribute('class', cls);
    (this.contentGroup || this.svg).appendChild(g);
    return g;
  }

  _el(tag, attrs) {
    const el = document.createElementNS('http://www.w3.org/2000/svg', tag);
    for (const [k, v] of Object.entries(attrs)) {
      el.setAttribute(k, v);
    }
    return el;
  }
}
