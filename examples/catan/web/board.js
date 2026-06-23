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
const PORT_COLORS = {
  lumber: '#2d5a27',
  brick: '#b85c38',
  wool: '#7ec850',
  grain: '#e8b430',
  ore: '#7a7a7a',
  generic: '#ffffff',
};
const TERRAIN_TEXTURES = {
  forest: {
    base: '#2d5a27',
    strokes: [
      ['path', { d: 'M 2 18 L 8 6 L 14 18 Z M 14 18 L 21 4 L 28 18 Z', fill: 'none', stroke: '#163f1d', 'stroke-width': 1.4 }],
      ['path', { d: 'M 5 23 H 25', fill: 'none', stroke: '#3d7a34', 'stroke-width': 1, opacity: 0.55 }],
    ],
  },
  hills: {
    base: '#b85c38',
    strokes: [
      ['path', { d: 'M -2 19 C 7 8, 16 8, 31 18', fill: 'none', stroke: '#8d3f2a', 'stroke-width': 1.5 }],
      ['path', { d: 'M 2 25 C 10 17, 19 17, 29 24', fill: 'none', stroke: '#d27a50', 'stroke-width': 1.2, opacity: 0.65 }],
    ],
  },
  pasture: {
    base: '#7ec850',
    strokes: [
      ['path', { d: 'M 4 22 C 7 16, 10 16, 12 22 M 15 20 C 18 13, 22 13, 25 20', fill: 'none', stroke: '#4d9a3f', 'stroke-width': 1.3 }],
      ['circle', { cx: 7, cy: 8, r: 1.5, fill: '#b8e58c', opacity: 0.75 }],
      ['circle', { cx: 22, cy: 12, r: 1.2, fill: '#d8f4ad', opacity: 0.65 }],
    ],
  },
  fields: {
    base: '#e8b430',
    strokes: [
      ['path', { d: 'M 5 0 V 30 M 13 0 V 30 M 21 0 V 30', fill: 'none', stroke: '#bd7f1f', 'stroke-width': 1.1, opacity: 0.65 }],
      ['path', { d: 'M 2 8 H 28 M 0 20 H 30', fill: 'none', stroke: '#f7d66d', 'stroke-width': 1.2, opacity: 0.75 }],
    ],
  },
  mountains: {
    base: '#7a7a7a',
    strokes: [
      ['path', { d: 'M 1 24 L 10 7 L 16 24 M 12 24 L 21 4 L 30 24', fill: 'none', stroke: '#4e5358', 'stroke-width': 1.6 }],
      ['path', { d: 'M 10 7 L 13 13 L 16 8 M 21 4 L 24 12 L 27 8', fill: 'none', stroke: '#c8c8c8', 'stroke-width': 1, opacity: 0.8 }],
    ],
  },
  desert: {
    base: '#d4c088',
    strokes: [
      ['path', { d: 'M 0 10 C 8 6, 16 14, 30 9 M -2 22 C 8 17, 17 25, 32 19', fill: 'none', stroke: '#b69b62', 'stroke-width': 1.2, opacity: 0.7 }],
      ['circle', { cx: 8, cy: 18, r: 1, fill: '#ead7a0', opacity: 0.8 }],
      ['circle', { cx: 23, cy: 4, r: 0.9, fill: '#a98c56', opacity: 0.55 }],
    ],
  },
};

const HEX_SIZE = 50;
const SQRT3 = Math.sqrt(3);
const BUILDING_SCALE = 1.5;
const SETTLEMENT_ICON_WIDTH = 12 * BUILDING_SCALE;
const SETTLEMENT_ICON_HEIGHT = 11 * BUILDING_SCALE;
const CITY_ICON_WIDTH = 15 * BUILDING_SCALE;
const CITY_ICON_HEIGHT = 14 * BUILDING_SCALE;
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
    this.onPortClick = null;
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

    const defs = this._el('defs', {});
    this.svg.appendChild(defs);
    this._defineTileTextures(defs);

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

    // Port editor click targets must sit above tile hit polygons.
    const portHitsG = this._g('port-hit-targets');
    if (this.onPortClick) {
      for (const port of board.ports) {
        this._drawPortHit(portHitsG, port, board.nodes);
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
        const settlement = this._el('polygon', {
          points: this._settlementPoints(x, y),
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
      el = this._el('polygon', {
        points: this._settlementPoints(x, y),
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
      fill: terrain ? `url(#terrain-texture-${terrain})` : color,
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

  _defineTileTextures(defs) {
    for (const [terrain, texture] of Object.entries(TERRAIN_TEXTURES)) {
      const pattern = this._el('pattern', {
        id: `terrain-texture-${terrain}`,
        patternUnits: 'userSpaceOnUse',
        width: 30,
        height: 30,
      });
      pattern.appendChild(this._el('rect', {
        x: 0, y: 0, width: 30, height: 30, fill: texture.base,
      }));
      for (const [tag, attrs] of texture.strokes) {
        pattern.appendChild(this._el(tag, attrs));
      }
      defs.appendChild(pattern);
    }
  }

  _portGeometry(port, nodes) {
    const [n0, n1] = port.nodes;
    const [x0, y0] = nodes[n0];
    const [x1, y1] = nodes[n1];
    const mx = (x0 + x1) / 2;
    const my = (y0 + y1) / 2;

    if (!this._centroid) {
      let sx = 0, sy = 0;
      for (const [x, y] of nodes) { sx += x; sy += y; }
      this._centroid = [sx / nodes.length, sy / nodes.length];
    }
    const [cx, cy] = this._centroid;
    const ex = x1 - x0, ey = y1 - y0;
    let nx = -ey, ny = ex;
    const toCenterX = cx - mx, toCenterY = cy - my;
    if (nx * toCenterX + ny * toCenterY > 0) { nx = -nx; ny = -ny; }
    const nlen = Math.sqrt(nx * nx + ny * ny) || 1;
    const offset = 18;
    const lx = mx + nx / nlen * offset;
    const ly = my + ny / nlen * offset;

    return { x0, y0, x1, y1, lx, ly };
  }

  _drawPort(parent, port, nodes) {
    const { x0, y0, x1, y1, lx, ly } = this._portGeometry(port, nodes);
    const color = PORT_COLORS[port.kind] || PORT_COLORS.generic;
    const ratio = port.kind === 'generic' ? '3:1' : '2:1';
    const darkText = port.kind === 'grain' || port.kind === 'wool' || port.kind === 'generic';
    const group = this._el('g', { class: port.selected ? 'port-marker selected' : 'port-marker' });
    parent.appendChild(group);

    group.appendChild(this._el('path', {
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
    group.appendChild(this._el('circle', {
      cx: lx, cy: ly, r: 10,
      fill: isGeneric ? '#334' : color,
      stroke: port.selected ? '#e94560' : (isGeneric ? color : 'none'),
      'stroke-width': port.selected ? 2.5 : 1.5,
      opacity: 0.85
    }));
    const txt = this._el('text', {
      x: lx, y: ly + 3,
      'text-anchor': 'middle', 'font-size': '8',
      fill: isGeneric ? '#ddd' : (darkText ? '#222' : '#fff'),
      'font-weight': '600'
    });
    txt.textContent = ratio;
    group.appendChild(this._keepUpright(txt, lx, ly));
  }

  _drawPortHit(parent, port, nodes) {
    if (!Number.isInteger(port.index)) return;
    const { lx, ly } = this._portGeometry(port, nodes);
    const ratio = port.kind === 'generic' ? '3:1' : '2:1';
    const hit = this._el('circle', {
      cx: lx, cy: ly, r: 18,
      fill: 'transparent',
      cursor: 'pointer',
      'pointer-events': 'all',
    });
    hit.addEventListener('click', (event) => {
      event.stopPropagation();
      this.onPortClick?.(port.index);
    });
    this._attachTooltip(hit, `Port ${port.index + 1}: ${ratio}`);
    parent.appendChild(hit);
  }

  _playerColor(playerIndex) {
    return playerIndex === 0 ? '#4a9eff' : '#ff6b6b';
  }

  _settlementPoints(x, y) {
    const w = SETTLEMENT_ICON_WIDTH;
    const h = SETTLEMENT_ICON_HEIGHT;
    const left = x - w / 2;
    const top = y - h / 2;
    return [
      [left + w * 0.5, top],
      [left + w, top + h * 0.43],
      [left + w, top + h],
      [left, top + h],
      [left, top + h * 0.43],
    ].map(([px, py]) => `${px},${py}`).join(' ');
  }

  _cityPoints(x, y) {
    const w = CITY_ICON_WIDTH;
    const h = CITY_ICON_HEIGHT;
    const left = x - w / 2;
    const top = y - h / 2;
    return [
      [left, top + h],
      [left, top + h * 0.38],
      [left + w * 0.18, top + h * 0.38],
      [left + w * 0.18, top + h * 0.16],
      [left + w * 0.38, top + h * 0.16],
      [left + w * 0.38, top + h * 0.38],
      [left + w * 0.62, top + h * 0.38],
      [left + w * 0.62, top + h * 0.16],
      [left + w * 0.82, top + h * 0.16],
      [left + w * 0.82, top + h * 0.38],
      [left + w, top + h * 0.38],
      [left + w, top + h],
    ].map(([px, py]) => `${px},${py}`).join(' ');
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
